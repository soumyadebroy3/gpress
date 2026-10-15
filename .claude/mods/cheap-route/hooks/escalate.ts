// Decides, from one step of a cheap-model subagent, whether the task has
// outgrown the cheap model. Once it has, the subagent finishes on the
// session's model, starting by redoing the step that tripped the check.

import type { TurnStepResult } from 'claude-code'

const WRITING_TOOLS = new Set(['Edit', 'MultiEdit', 'Write', 'NotebookEdit'])

// Shell commands that change the checkout or the outside world.
const MUTATING_SHELL =
  /(^|[;&|]\s*)(git\s+(commit|push|merge|rebase|reset|checkout|apply|am|cherry-pick|stash|tag)|rm|mv|cp|mkdir|touch|tee|patch|cargo\s+(fmt|fix|add|update|publish)|npm\s+(install|i|publish|version)|pip\s+install)\b|sed\s+-i|perl\s+-pi|\s>{1,2}\s*[^&\s]/

const UNSURE =
  /\b(i'?m not sure|not certain|i (could|can)n'?t (find|determine|tell)|could not (find|determine|locate)|unable to|i don'?t know|unclear (whether|if|what)|needs? (more|further) (investigation|analysis))\b/i

export const outgrown = (r: TurnStepResult, index: number, maxCheapSteps: number): string | undefined => {
  if (r.stopReason === null || r.stopReason === 'refusal' || r.stopReason === 'max_tokens') {
    return `cheap model stopped (${r.stopReason ?? 'no response'})`
  }
  for (const use of r.toolUses) {
    if (WRITING_TOOLS.has(use.name)) return `wants to change files (${use.name})`
    if (use.name === 'Bash') {
      const command = String((use.input as { command?: unknown } | null)?.command ?? '')
      if (MUTATING_SHELL.test(command)) return 'wants to run a command that changes things'
    }
  }
  if (r.toolUses.length === 0 && r.answer.trim().length === 0) return 'empty answer'
  if (UNSURE.test(r.answer)) return 'cheap model was unsure'
  if (index >= maxCheapSteps) return `task ran past ${maxCheapSteps} steps`
  return undefined
}
