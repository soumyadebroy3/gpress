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

const num = (v: unknown): string | undefined =>
  typeof v === 'number' || (typeof v === 'string' && /^\d+$/.test(v)) ? String(v) : undefined

const key = (repo: string | undefined, pr: string): PrKey => `${repo ? repo.toLowerCase() : HERE}#${pr}`

const verdictOf = (body: string): Verdict | undefined => {
  // A body passed as "$(cat <<'EOF' ... EOF)" starts after the heredoc line.
  const m = VERDICT_AT_START.exec(body.replace(/^\$\(cat\s+<<-?\s*['"]?\w+['"]?[ \t]*\n/, ''))
  return m ? { pass: m[1] === 'PASS', sha: m[2]! } : undefined
}

// The simple commands of a shell line: split at ; & | newlines and
// backticks outside quotes, so one command's text never speaks for another.
const segments = (command: string): string[] => {
  const out: string[] = []
  let cur = ''
  let quote = ''
  for (const ch of command) {
    if (quote) {
      if (ch === quote) quote = ''
      cur += ch
    } else if (ch === '"' || ch === "'") {
      quote = ch
      cur += ch
    } else if (';&|\n`'.includes(ch)) {
      if (cur.trim()) out.push(cur)
      cur = ''
    } else cur += ch
  }
  if (cur.trim()) out.push(cur)
  return out
}

const unquoted = (segment: string): string => segment.replace(/(["'])[\s\S]*?\1/g, ' ')

// A command with its quoted text taken out, so words inside a message are not
// read as the command's own. A shell started with -c, or eval, keeps its
// quoted text: there that text is the command.
const bare = (segment: string): string =>
  /(?:^|\s)(?:\S*\/)?(?:bash|sh|zsh)\s+-c\b|(?:^|\s)eval\s/.test(unquoted(segment)) ? segment : unquoted(segment)

const repoIn = (segment: string): string | undefined =>
  /\s(?:-R\s*|--repo[= ])([^\s'"]+\/[^\s'"]+)/.exec(` ${segment}`)?.[1] ?? /\bGH_REPO=([^\s'"]+)/.exec(segment)?.[1]

const numberAfter = (text: string, word: RegExp): string | undefined => {
  const m = word.exec(text)
  if (!m) return undefined
  const rest = text.slice(m.index + m[0].length)
  return /(?:^|\s)(\d+)(?=\s|$)/.exec(rest)?.[1] ?? /\/pull\/(\d+)/.exec(rest)?.[1]
}

// A comment whose first line is a verdict: which pull request it is on, and
// the verdict. Only a command that posts a comment counts, never one that reads.
export const postedVerdict = (tool: string, input: Record<string, unknown>): { pr: PrKey; verdict: Verdict } | undefined => {
  if (/(^|__)add_issue_comment$/.test(tool)) {
    const pr = num(input.issue_number)
    const verdict = verdictOf(String(input.body ?? ''))
    const repo = typeof input.owner === 'string' && typeof input.repo === 'string' ? `${input.owner}/${input.repo}` : undefined
    return pr && verdict ? { pr: key(repo, pr), verdict } : undefined
  }
  if (tool !== 'Bash') return undefined
  for (const segment of segments(String(input.command ?? ''))) {
    const plain = unquoted(segment)
    if (/\bgh\b[\s\S]*\bpr\b[\s\S]*\bcomment\b/.test(plain)) {
      const body = /(?:--body|-b)[= ]\s*(["'])([\s\S]*?)\1/.exec(segment)?.[2]
      const verdict = body === undefined ? undefined : verdictOf(body)
      const pr = numberAfter(plain, /\bcomment\b/)
      if (verdict && pr) return { pr: key(repoIn(segment), pr), verdict }
    }
    const api = /\bgh\b[\s\S]*\bapi\b[\s\S]*?(?:repos\/([^/\s]+\/[^/\s]+)\/)?issues\/(\d+)\/comments\b/.exec(plain)
    if (api && !/(?:-X|--method)[= ]?\s*(GET|PATCH|DELETE)\b/i.test(plain)) {
      const body = /-[fF]\s+body=(["'])([\s\S]*?)\1/.exec(segment)?.[2]
      const verdict = body === undefined ? undefined : verdictOf(body)
      if (verdict) return { pr: key(api[1], api[2]!), verdict }
    }
  }
  return undefined
}

export type MergeTarget = { pr?: PrKey; sha?: string; why?: string }

// A call that merges a pull request, turns auto-merge on, or pushes to main.
// `why` is set when the call is a merge the gate cannot tie to one pull
// request and one head. It matches broadly: a refused command that was not a
// merge costs a retry, a merge that slips through costs the review.
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
  for (const segment of segments(String(input.command ?? ''))) {
    const plain = bare(segment)
    if (/\bgh\b[\s\S]*\bpr\b[\s\S]*\bmerge\b/.test(plain) && !/\bcomment\b/.test(plain)) {
      if (/--disable-auto\b/.test(plain)) continue
      const pr = numberAfter(plain, /\bmerge\b/)
      const urlRepo = /github\.com\/([^/\s]+\/[^/\s]+)\/pull\//.exec(plain)?.[1]
      // Read off the command itself, where a quoted sha keeps its quotes.
      const sha = new RegExp(`--match-head-commit[= ]["']?${SHA}["']?(?=\\s|$)`).exec(segment)?.[1]
      return pr ? { pr: key(repoIn(segment) ?? urlRepo, pr), sha } : { why: 'name the pull request by number (`gh pr merge <n>`).' }
    }
    if (/\bgh\b[\s\S]*\balias\b/.test(plain) && /\bmerge\b/.test(segment)) return { why: 'no gh alias for merging; use `gh pr merge <n>`.' }
    if (/\bgh\b[\s\S]*\bapi\b/.test(plain) && /graphql/.test(segment) && /mergePullRequest|enablePullRequestAutoMerge|query=@/.test(segment)) {
      return { why: 'merge through `gh pr merge <n> --match-head-commit <sha>` or the merge tool, not GraphQL.' }
    }
    const route = /pulls\/([^/\s'"]+)\/(?:merge|ccr\/auto_merge)\b/.exec(segment)
    if (route && /\bcurl\b/.test(plain)) return { why: 'merge through `gh pr merge <n> --match-head-commit <sha>`, not curl.' }
    if (route && /\bgh\b[\s\S]*\bapi\b/.test(plain) && /(?:-X|--method)[= ]?\s*PUT\b/i.test(plain)) {
      const pr = num(route[1])
      const repo = /repos\/([^/\s]+\/[^/\s]+)\/pulls\//.exec(segment)?.[1]
      const sha = new RegExp(`-[fF]\\s+sha=${SHA}`).exec(segment)?.[1]
      return pr ? { pr: key(repo, pr), sha } : { why: 'name the pull request by number in the merge route.' }
    }
    const push = plain.search(/\bpush\b/)
    if (/\bgit\b/.test(plain) && push >= 0 && /(?:^|\s)\+?(?:\S*:)?(?:refs\/heads\/)?(main|master)(?=\s|$)/.test(plain.slice(push))) {
      return { why: 'changes go to main through a reviewed pull request, not a direct push.' }
    }
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
