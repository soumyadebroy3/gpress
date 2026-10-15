import { test, expect } from 'claude-code/testing'

const SHA = '24fa28dd2e415f2ec42e1b708f03ebcc41c44bb6'
const OTHER = 'ecff299a4043c8590b1d61b4b1e447757b0813e5'

// Beneath the plugin every tool call succeeds.
const ok = (on: any) => on('tool.call', () => ({ result: 'ok', isError: false }))

const comment = ($: any, pr: number, body: string) =>
  $.tool.call({ tool: 'mcp__github__add_issue_comment', owner: 'soumyadebroy3', repo: 'gpress', issue_number: pr, body } as any)

const merge = ($: any, pr: number, sha?: string) =>
  $.tool.call({ tool: 'mcp__github__merge_pull_request', owner: 'soumyadebroy3', repo: 'gpress', pullNumber: pr, ...(sha ? { expectedHeadSha: sha } : {}) } as any)

const denied = (r: any) => r.isError === true || r.deny !== undefined

test('a merge with no review is refused', async ($, on) => {
  ok(on)
  const r: any = await merge($, 5, SHA)
  expect(denied(r)).toBe(true)
  expect(JSON.stringify(r)).toContain('/review-pr 5')
})

test('a merge after a PASS of the same head goes through', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA}\n- fmt, clippy, tests: pass`)
  expect(denied(await merge($, 5, SHA))).toBe(false)
})

test('a short sha in the verdict matches the full head', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA.slice(0, 7)}`)
  expect(denied(await merge($, 5, SHA))).toBe(false)
})

test('a merge after FINDINGS is refused', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: FINDINGS ${SHA}\n- blocker src/flate/fast.rs:40`)
  const r: any = await merge($, 5, SHA)
  expect(denied(r)).toBe(true)
  expect(JSON.stringify(r)).toContain('found blockers')
})

test('a push after the review means a new review', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA}`)
  const r: any = await merge($, 5, OTHER)
  expect(denied(r)).toBe(true)
  expect(JSON.stringify(r)).toContain('Review the new head')
})

test('a review of another pull request does not count', async ($, on) => {
  ok(on)
  await comment($, 6, `gpress-review: PASS ${SHA}`)
  expect(denied(await merge($, 5, SHA))).toBe(true)
})

test('gh pr merge in the shell is gated too', async ($, on) => {
  ok(on)
  const before: any = await $.tool.call({ tool: 'Bash', command: 'gh pr merge 7 --squash' } as any)
  expect(denied(before)).toBe(true)
  await $.tool.call({ tool: 'Bash', command: `gh pr comment 7 --body "gpress-review: PASS ${SHA}"` } as any)
  const after: any = await $.tool.call({ tool: 'Bash', command: 'gh pr merge 7 --squash' } as any)
  expect(denied(after)).toBe(false)
})

test('other tool calls pass untouched', async ($, on) => {
  ok(on)
  expect(denied(await $.tool.call({ tool: 'Bash', command: 'cargo fmt --all --check' } as any))).toBe(false)
})

test('the gate can be turned off', { options: { requireReview: false } }, async ($, on) => {
  ok(on)
  expect(denied(await merge($, 5, SHA))).toBe(false)
})

test('/review-pr needs a number', async ($, on) => {
  on('prompt.submit', (_$: any, e: any) => ({ text: e.text }))
  const r: any = await $.command.run({ command: 'review-pr', args: '' } as any)
  expect(r.text).toContain('Usage')
})

test('/review-pr sends the protocol to the model once its turn ends', async ($, on) => {
  const sent: string[] = []
  on('prompt.submit', (_$: any, e: any) => { sent.push(e.text); return { text: e.text } })
  on('turn.complete', () => ({ text: '' }))
  const r: any = await $.command.run({ command: 'review-pr', args: '12' } as any)
  expect(r.text).toContain('#12')
  await $.turn.complete({ turnId: 't1', reason: 'answer', answer: 'done', durationMs: 1 } as any)
  expect(sent.join('')).toContain('Review gpress pull request #12')
  expect(sent.join('')).toContain('gpress-review: PASS <head sha>')
})
