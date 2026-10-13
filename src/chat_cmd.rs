//! `ai-orchestra chat` (and bare `ai-orchestra` in a terminal): the terminal around [`ChatEngine`].
//!
//! All behaviour lives in `ai_orchestra::ux::chat`; this file only reads lines, prints, and turns
//! Ctrl-C into a cancellation of the consultation that is running (at the prompt it leaves).

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use ai_orchestra::Config;
use ai_orchestra::Orchestrator;
use ai_orchestra::error::OrchestraError;
use ai_orchestra::eval::feedback::FeedbackStore;
use ai_orchestra::ux::chat::{ChatEngine, ChatIo, Flow};
use ai_orchestra::ux::prefs;
use ai_orchestra::ux::settings::Key;
use tokio_util::sync::CancellationToken;

struct TerminalIo {
    interrupt: Option<tokio::task::JoinHandle<()>>,
}

impl ChatIo for TerminalIo {
    fn out(&mut self, text: &str) {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        let _ = std::io::stdout().flush();
    }

    fn status(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn begin_consultation(&mut self) -> CancellationToken {
        let cancel = CancellationToken::new();
        let on_interrupt = cancel.clone();
        self.interrupt = Some(tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                on_interrupt.cancel();
            }
        }));
        cancel
    }

    fn end_consultation(&mut self) {
        if let Some(h) = self.interrupt.take() {
            h.abort();
        }
    }
}

/// Runs the chat until `/exit`, end of input, or Ctrl-C at the prompt.
pub async fn run(
    orchestrator: Orchestrator,
    config: Config,
    session: Option<String>,
    presets: Vec<(Key, String)>,
) -> Result<u8, OrchestraError> {
    let interactive = std::io::stdin().is_terminal();
    let feedback = ai_orchestra::eval::feedback_path(&config).map(FeedbackStore::new);
    let mut engine = ChatEngine::new(
        Arc::new(orchestrator),
        config.clone(),
        prefs::path_for(&config),
        feedback,
        session,
    )?;
    let mut io = TerminalIo { interrupt: None };
    for (key, value) in presets {
        engine.preset(key, &value)?;
    }
    io.out(&engine.banner());

    // A thread reads lines so the prompt can be interrupted without losing input.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .send(line.trim_end_matches(['\n', '\r']).to_string())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    loop {
        if interactive {
            print!("\nYou: ");
            let _ = std::io::stdout().flush();
        }
        let line = tokio::select! {
            line = rx.recv() => line,
            _ = tokio::signal::ctrl_c() => {
                println!();
                None
            }
        };
        let Some(line) = line else { break };
        if engine.handle(&line, &mut io).await == Flow::Exit {
            break;
        }
    }
    if interactive {
        println!("Bye.");
    }
    Ok(0)
}
