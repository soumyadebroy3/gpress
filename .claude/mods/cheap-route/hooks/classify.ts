// Decides from a subagent's prompt whether the task is easy enough for a
// cheaper model. Hard signals always win, so a mixed task stays put.

export type Verdict = { easy: boolean; reason: string }

const HARD: Array<[RegExp, string]> = [
  [/\b(implement|build|design|architect|prototype)\w*/i, 'builds something'],
  [/\b(debug|root[- ]cause|diagnos\w*|investigat\w*|why does|why is|race condition|deadlock)\b/i, 'debugging'],
  [/\bfix(es|ing)?\b.*\b(bug|crash|fail\w*|regression|leak|error)\b/i, 'fixes a bug'],
  [/\b(optimi[sz]\w*|speed ?up|faster|performance|profil\w*|benchmark\w*)\b/i, 'performance work'],
  [/\b(refactor\w*|rewrite|migrat\w*|port (it|this|the))\b/i, 'restructures code'],
  [/\b(review|audit|security|vulnerab\w*|verify (that|the|correct)\w*|prove)\b/i, 'needs judgement'],
  [/\b(write|add) (unit |integration )?tests?\b/i, 'writes tests'],
  [/\b(plan|strategy|trade-?offs?|compare approaches|propose)\b/i, 'planning'],
]

const EASY: Array<[RegExp, string]> = [
  [/^\s*(find|locate|search|grep|look ?up|list|enumerate|count)\b/i, 'lookup'],
  [/\b(find|locate|search for|grep for|look up|list (all|every|the))\b/i, 'lookup'],
  [/\b(summari[sz]e|summary of|tl;?dr|skim)\b/i, 'summary'],
  [/\b(check|report) (the )?(status|whether|if|state)\b/i, 'status check'],
  [/\b(git (status|log|diff --stat)|ci status|is .* (green|passing|merged))\b/i, 'status check'],
  [/\b(rename|typo|reformat|format the|bump (the )?version|update (the )?(changelog|readme|docs?|comment))\b/i, 'mechanical edit'],
  [/\b(read|open|print|show|fetch|extract) (the |all |every )?(file|files|contents?|lines?|log|output|value)\b/i, 'read'],
  [/\b(which files?|where is|what (is|are) the (name|path|version|value))\b/i, 'lookup'],
]

export const classify = (prompt: string, subagentType: string, maxEasyChars: number): Verdict => {
  if (prompt.length > maxEasyChars) return { easy: false, reason: 'long prompt' }
  for (const [re, why] of HARD) if (re.test(prompt)) return { easy: false, reason: why }
  if (subagentType === 'Explore') return { easy: true, reason: 'read-only search agent' }
  for (const [re, why] of EASY) if (re.test(prompt)) return { easy: true, reason: why }
  return { easy: false, reason: 'no easy signal' }
}
