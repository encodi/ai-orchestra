// The engine asks the agents for section headings in English (it reads the answer back by those names), so the
// interface translates the known ones for display. The stored answer and its exports keep the original text.
export const ES_HEADINGS: Record<string, string> = {
  'answer': 'Respuesta',
  'recommendation': 'Recomendación',
  'recommended solution': 'Solución recomendada',
  'alternatives and criteria': 'Alternativas y criterios',
  'alternatives compared': 'Alternativas comparadas',
  'evaluation criteria': 'Criterios de evaluación',
  'strengths and weaknesses': 'Fortalezas y debilidades',
  'trade-offs, costs and risks': 'Compromisos, costos y riesgos',
  'areas of agreement': 'Áreas de acuerdo',
  'significant disagreements': 'Desacuerdos importantes',
  'assumptions': 'Supuestos',
  'evidence limitations': 'Límites de la evidencia',
  'central question': 'Pregunta central',
  'established facts': 'Hechos establecidos',
  'hypotheses and open questions': 'Hipótesis y preguntas abiertas',
  'opinions and contested claims': 'Opiniones y afirmaciones en disputa',
  'contradictions': 'Contradicciones',
  'outdated or unverified information': 'Información desactualizada o sin verificar',
  'lesson': 'Lección',
  'prerequisites': 'Requisitos previos',
  'corrections made after review': 'Correcciones hechas tras la revisión',
  'technical reasoning': 'Razonamiento técnico',
  'unresolved risks': 'Riesgos sin resolver',
  'suggested next steps': 'Próximos pasos sugeridos',
  'final synthesis': 'Síntesis final',
  'independent perspectives': 'Perspectivas independientes',
}

export function translateHeading(text: string, table: Record<string, string> | undefined): string {
  if (!table) return text
  const key = text.trim().replace(/[:.]+$/, '').toLowerCase()
  return table[key] ?? text
}
