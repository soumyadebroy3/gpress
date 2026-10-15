// Reads review verdicts out of the comments this session posts, and the pull
// request a merge call names, so a merge can wait for a passing review.

import { MARK } from './protocol'

export type Verdict = { pass: boolean; sha: string }

// Which pull request: `owner/repo#n`, or `here#n` when a gh command names no
// repository and so means the checkout's own.
export type PrKey = string

const HERE = 'here'
const SHA = '([0-9a-f]{7,40})'
const VERDICT_AT_START = new RegExp(`^\\s*${MARK.replace(/[-:]/g, '\\$&')}\\s*(PASS|FINDINGS)\\s+${SHA}(?=\\s|$)`)
// A gh command at the start of the line or after a shell separator, not one
// quoted inside another command.
const GH = String.raw`(?:^|[;&|(]\s*|\n\s*)gh\s+(?:-R\s+(\S+)\s+|--repo[= ](\S+)\s+)?`

const num = (v: unknown): string | undefined =>
  typeof v === 'number' || (typeof v === 'string' && /^\d+$/.test(v)) ? String(v) : undefined

const key = (repo: string | undefined, pr: string): PrKey => `${repo ? repo.toLowerCase() : HERE}#${pr}`

const verdictOf = (body: string): Verdict | undefined => {
  // A body passed as "$(cat <<'EOF' ... EOF)" starts after the heredoc line.
  const m = VERDICT_AT_START.exec(body.replace(/^\$\(cat\s+<<-?\s*['"]?\w+['"]?[ \t]*\n/, ''))
  return m ? { pass: m[1] === 'PASS', sha: m[2]! } : undefined
}

// The body text a shell command posts, when it posts one.
const shellBody = (command: string): string | undefined =>
  /(?:--body|-b)[= ]\s*(["'])([\s\S]*?)\1/.exec(command)?.[2] ?? /-[fF]\s+body=(["'])([\s\S]*?)\1/.exec(command)?.[2]

// A comment whose first line is a verdict: which pull request it is on, and
// the verdict. Only calls that post a comment count, never ones that read.
export const postedVerdict = (tool: string, input: Record<string, unknown>): { pr: PrKey; verdict: Verdict } | undefined => {
  if (/(^|__)add_issue_comment$/.test(tool)) {
    const pr = num(input.issue_number)
    const verdict = verdictOf(String(input.body ?? ''))
    const repo = typeof input.owner === 'string' && typeof input.repo === 'string' ? `${input.owner}/${input.repo}` : undefined
    return pr && verdict ? { pr: key(repo, pr), verdict } : undefined
  }
  if (tool !== 'Bash') return undefined
  const command = String(input.command ?? '')
  const body = shellBody(command)
  const verdict = body === undefined ? undefined : verdictOf(body)
  if (!verdict) return undefined
  const prComment = new RegExp(`${GH}pr\\s+(?:-R\\s+(\\S+)\\s+)?comment\\b`).exec(command)
  if (prComment) {
    // The number may come before or after the flags; the body is cut out first.
    const args = command.slice(prComment.index + prComment[0].length).replace(/(["'])[\s\S]*?\1/g, '')
    const pr = /^[^;&|\n]*?(?:^|\s)(\d+)(?=\s|$)/.exec(args)?.[1]
    return pr ? { pr: key(prComment[1] ?? prComment[2] ?? prComment[3], pr), verdict } : undefined
  }
  const api = new RegExp(`${GH}api\\s+(?:-X\\s+POST\\s+)?(?:/?repos/([^/\\s]+/[^/\\s]+)/)issues/(\\d+)/comments\\b`).exec(command)
  if (api && !/-X\s+(GET|PATCH|DELETE)/.test(command)) return { pr: key(api[3], api[4]!), verdict }
  return undefined
}

export type MergeTarget = { pr?: PrKey; sha?: string; why?: string }

// A call that merges a pull request, turns auto-merge on, or pushes to main.
// `why` is set when the call is a merge the gate cannot tie to one pull
// request and one head.
export const mergeTarget = (tool: string, input: Record<string, unknown>): MergeTarget | undefined => {
  if (/(^|__)enable_pr_auto_merge$/.test(tool)) {
    return { why: 'auto-merge cannot pin the reviewed head; merge directly with expectedHeadSha once CI is green.' }
  }
  if (/(^|__)merge_pull_request$/.test(tool)) {
    const pr = num(input.pullNumber)
    const repo = typeof input.owner === 'string' && typeof input.repo === 'string' ? `${input.owner}/${input.repo}` : undefined
    const sha = typeof input.expectedHeadSha === 'string' ? input.expectedHeadSha : undefined
    return pr ? { pr: key(repo, pr), sha } : { why: 'the merge names no pull request number.' }
  }
  if (/(^|__)(push_files|create_or_update_file|delete_file)$/.test(tool) && /^(main|master)$/.test(String(input.branch ?? ''))) {
    return { why: 'changes go to main through a reviewed pull request, not a direct write.' }
  }
  if (tool !== 'Bash') return undefined
  const command = String(input.command ?? '')
  const sha = new RegExp(`--match-head-commit[= ]${SHA}`).exec(command)?.[1] ?? new RegExp(`\\bsha=${SHA}`).exec(command)?.[1]

  const prMerge = new RegExp(`${GH}pr\\s+(?:-R\\s+(\\S+)\\s+)?merge\\b([^;&|\\n]*)`).exec(command)
  if (prMerge && !/--disable-auto\b/.test(prMerge[4]!)) {
    const repo = prMerge[1] ?? prMerge[2] ?? prMerge[3]
    const rest = prMerge[4]!
    const pr = /(?:^|\s)(\d+)(?=\s|$)/.exec(rest)?.[1] ?? /\/pull\/(\d+)/.exec(rest)?.[1]
    const urlRepo = /github\.com\/([^/\s]+\/[^/\s]+)\/pull\//.exec(rest)?.[1]
    return pr ? { pr: key(repo ?? urlRepo, pr), sha } : { why: 'name the pull request by number (`gh pr merge <n>`).' }
  }
  const apiMerge = new RegExp(`${GH}api\\b[^;&|\\n]*?(?:repos/([^/\\s]+/[^/\\s]+)/)?pulls/([^/\\s]+)/(?:merge|ccr/auto_merge)\\b`).exec(command)
  if (apiMerge && /(?:-X|--method)[= ]?\s*PUT\b/i.test(command)) {
    const pr = num(apiMerge[4])
    return pr ? { pr: key(apiMerge[3], pr), sha } : { why: 'name the pull request by number in the merge route.' }
  }
  if (new RegExp(`${GH}api\\s+graphql\\b`).test(command) && /mergePullRequest|enablePullRequestAutoMerge/.test(command)) {
    return { why: 'merge through `gh pr merge <n> --match-head-commit <sha>` or the merge tool, not GraphQL.' }
  }
  if (/(?:^|[;&|(]\s*|\n\s*)git\s+push\b[^;&|\n]*\s(?:\S+:)?(?:refs\/heads\/)?(main|master)\b/.test(command)) {
    return { why: 'changes go to main through a reviewed pull request, not a direct push.' }
  }
  return undefined
}

const same = (a: string, b: string) => a.startsWith(b) || b.startsWith(a)

// The verdict for a pull request: its own key first, then the same number in
// the checkout's repository when either side named none.
export const findVerdict = (verdicts: ReadonlyMap<PrKey, Verdict>, pr: PrKey): Verdict | undefined => {
  const exact = verdicts.get(pr)
  if (exact) return exact
  const n = pr.slice(pr.indexOf('#') + 1)
  return pr.startsWith(`${HERE}#`) ? [...verdicts].find(([k]) => k.endsWith(`#${n}`))?.[1] : verdicts.get(`${HERE}#${n}`)
}

// Why a merge must wait, or undefined when the review allows it.
export const mergeBlocked = (target: MergeTarget, verdict: Verdict | undefined): string | undefined => {
  if (target.why || !target.pr) return target.why ?? 'the merge names no pull request.'
  const n = target.pr.slice(target.pr.indexOf('#') + 1)
  if (!verdict) return `no review of pull request #${n} has been posted in this session. Run /review-pr ${n} (the review protocol) first.`
  if (!verdict.pass) return `the last review of #${n} (${verdict.sha.slice(0, 7)}) found blockers. Fix them and review again.`
  if (!target.sha) return `pin the head the review passed (${verdict.sha.slice(0, 7)}): expectedHeadSha on the merge tool, or --match-head-commit with gh.`
  if (!same(target.sha, verdict.sha)) return `the passing review was of ${verdict.sha.slice(0, 7)}, but the merge pins ${target.sha.slice(0, 7)}. Review the new head first.`
  return undefined
}
