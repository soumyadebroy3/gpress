// Reads review verdicts out of the comments this session posts, and the pull
// request a merge call names, so a merge can wait for a passing review.

import { MARK } from './protocol'

export type Verdict = { pass: boolean; sha: string }

const VERDICT = new RegExp(`${MARK.replace(/[-:]/g, '\\$&')}\\s*(PASS|FINDINGS)\\s+([0-9a-f]{7,40})`)

const num = (v: unknown): string | undefined =>
  typeof v === 'number' || (typeof v === 'string' && /^\d+$/.test(v)) ? String(v) : undefined

// A comment holding a verdict: which pull request it is on, and the verdict.
export const postedVerdict = (tool: string, input: Record<string, unknown>): { pr: string; verdict: Verdict } | undefined => {
  const text = JSON.stringify(input)
  const m = VERDICT.exec(text.replace(/\\n/g, '\n'))
  if (!m) return undefined
  const verdict = { pass: m[1] === 'PASS', sha: m[2]! }
  if (tool === 'Bash') {
    const command = String(input.command ?? '')
    const pr = /gh\s+pr\s+(?:comment|review)\s+(\d+)/.exec(command)?.[1] ?? /(?:issues|pulls)\/(\d+)\/(?:comments|reviews)/.exec(command)?.[1]
    return pr ? { pr, verdict } : undefined
  }
  if (/^mcp__github__(add_issue_comment|pull_request_review_write)$/.test(tool)) {
    const pr = num(input.issue_number) ?? num(input.pullNumber)
    return pr ? { pr, verdict } : undefined
  }
  return undefined
}

// A call that merges a pull request (or turns auto-merge on): which one, and
// the head sha it pins, when it pins one.
export const mergeTarget = (tool: string, input: Record<string, unknown>): { pr: string; sha?: string } | undefined => {
  if (tool === 'mcp__github__merge_pull_request' || tool === 'mcp__github__enable_pr_auto_merge') {
    const pr = num(input.pullNumber)
    const sha = typeof input.expectedHeadSha === 'string' ? input.expectedHeadSha : undefined
    return pr ? { pr, sha } : undefined
  }
  if (tool === 'Bash') {
    const command = String(input.command ?? '')
    const pr = /gh\s+pr\s+merge\s+(\d+)/.exec(command)?.[1] ?? /pulls\/(\d+)\/(?:merge|ccr\/auto_merge)\b/.exec(command)?.[1]
    if (!pr) return undefined
    const sha = /--match-head-commit[= ]([0-9a-f]{7,40})/.exec(command)?.[1] ?? /\bsha=([0-9a-f]{7,40})/.exec(command)?.[1]
    return { pr, sha }
  }
  return undefined
}

const same = (a: string, b: string) => a.startsWith(b) || b.startsWith(a)

// Why a merge must wait, or undefined when the review allows it.
export const mergeBlocked = (target: { pr: string; sha?: string }, verdict: Verdict | undefined): string | undefined => {
  if (!verdict) return `no review of pull request #${target.pr} has been posted in this session. Run /review-pr ${target.pr} (the review protocol) first.`
  if (!verdict.pass) return `the last review of #${target.pr} (${verdict.sha.slice(0, 7)}) found blockers. Fix them and review again.`
  if (target.sha && !same(target.sha, verdict.sha)) return `the passing review was of ${verdict.sha.slice(0, 7)}, but the merge pins ${target.sha.slice(0, 7)}. Review the new head first.`
  return undefined
}
