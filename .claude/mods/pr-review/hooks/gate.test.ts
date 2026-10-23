import { test, expect, mock } from 'claude-code/testing'

const SHA = '24fa28dd2e415f2ec42e1b708f03ebcc41c44bb6'
const OTHER = 'ecff299a4043c8590b1d61b4b1e447757b0813e5'

// Beneath the plugin every tool call succeeds.
const ok = (on: any) => on('tool.call', () => ({ result: 'ok', isError: false }))

const comment = ($: any, pr: number, body: string, repo = 'gpress') =>
  $.tool.call({ tool: 'mcp__github__add_issue_comment', owner: 'soumyadebroy3', repo, issue_number: pr, body } as any)

const merge = ($: any, pr: number, sha?: string, repo = 'gpress') =>
  $.tool.call({ tool: 'mcp__github__merge_pull_request', owner: 'soumyadebroy3', repo, pullNumber: pr, ...(sha ? { expectedHeadSha: sha } : {}) } as any)

const bash = ($: any, command: string) => $.tool.call({ tool: 'Bash', command } as any)

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

test('a merge that pins no head is refused', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA}`)
  const r: any = await merge($, 5)
  expect(denied(r)).toBe(true)
  expect(JSON.stringify(r)).toContain('expectedHeadSha')
})

test('a merge after FINDINGS is refused', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: FINDINGS ${SHA}\n- blocker src/flate/fast.rs:40`)
  const r: any = await merge($, 5, SHA)
  expect(JSON.stringify(r)).toContain('found blockers')
})

test('a FINDINGS comment quoting a PASS line is not a pass', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: FINDINGS ${SHA}\nthe earlier "gpress-review: PASS ${SHA}" was wrong`)
  expect(denied(await merge($, 5, SHA))).toBe(true)
})

test('a push after the review means a new review', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA}`)
  const r: any = await merge($, 5, OTHER)
  expect(JSON.stringify(r)).toContain('Review the new head')
})

test('a review of another pull request or repository does not count', async ($, on) => {
  ok(on)
  await comment($, 6, `gpress-review: PASS ${SHA}`)
  await comment($, 5, `gpress-review: PASS ${SHA}`, 'other')
  expect(denied(await merge($, 5, SHA))).toBe(true)
})

test('auto-merge is refused even after a PASS', async ($, on) => {
  ok(on)
  await comment($, 5, `gpress-review: PASS ${SHA}`)
  const r: any = await $.tool.call({ tool: 'mcp__github__enable_pr_auto_merge', owner: 'soumyadebroy3', repo: 'gpress', pullNumber: 5 } as any)
  expect(denied(r)).toBe(true)
})

test('every gh merge form is gated', async ($, on) => {
  ok(on)
  for (const command of [
    'gh pr merge 7 --squash',
    'gh pr merge --squash 7',
    'gh -R soumyadebroy3/gpress pr merge 7',
    'gh pr -R soumyadebroy3/gpress merge 7',
    'gh pr merge --auto --squash --delete-branch 7',
    'gh pr merge https://github.com/soumyadebroy3/gpress/pull/7',
    'gh pr merge --auto --squash',
    'cd x && gh pr merge claude/branch',
    'N=7; gh api -X PUT repos/soumyadebroy3/gpress/pulls/$N/merge',
    'gh api -X PUT repos/soumyadebroy3/gpress/pulls/7/merge',
    'gh api graphql -f query="mutation { mergePullRequest(input: {}) { clientMutationId } }"',
    'git push origin HEAD:main',
    'gh pr --repo soumyadebroy3/gpress merge 7',
    ' gh pr merge 7',
    'if gh pr merge 7; then echo ok; fi',
    'GH_REPO=soumyadebroy3/gpress gh pr merge 7',
    'sudo /usr/bin/gh pr merge 7',
    'bash -c "gh pr merge 7"',
    'echo `gh pr merge 7`',
    'gh api -X POST graphql -f query="mutation { mergePullRequest(input: {}) { clientMutationId } }"',
    'gh api graphql -F query=@m.graphql',
    'curl -X PUT https://api.github.com/repos/soumyadebroy3/gpress/pulls/7/merge',
    'git -C . push origin main',
    "gh alias set m 'pr merge'",
  ]) {
    expect({ command, denied: denied(await bash($, command)) }).toEqual({ command, denied: true })
  }
})

test('a gh verdict comment unlocks a pinned gh merge', async ($, on) => {
  ok(on)
  await bash($, `gh pr comment 7 --body "gpress-review: PASS ${SHA}\n- all checks pass"`)
  expect(denied(await bash($, 'gh pr merge 7 --squash'))).toBe(true)
  expect(denied(await bash($, `gh pr merge 7 --squash --match-head-commit ${SHA}`))).toBe(false)
  expect(denied(await merge($, 7, SHA))).toBe(false)
})

test('a verdict with the number after the flags counts', async ($, on) => {
  ok(on)
  await bash($, `gh pr comment --body "gpress-review: PASS ${SHA} 12" 7`)
  expect(denied(await bash($, `gh pr merge 7 --match-head-commit ${SHA}`))).toBe(false)
})

test('a verdict must sit on the first line, with a whole sha', async ($, on) => {
  ok(on)
  await comment($, 5, `Summary first\n\ngpress-review: PASS ${SHA}`)
  await comment($, 6, `gpress-review: PASS deadbeefcafe-not-a-sha`)
  expect(denied(await merge($, 5, SHA))).toBe(true)
  expect(denied(await merge($, 6, 'deadbeefcafe'))).toBe(true)
})

test('a merge pins its head only with its own flag', async ($, on) => {
  ok(on)
  await comment($, 7, `gpress-review: PASS ${SHA}`)
  expect(denied(await bash($, `gh pr merge 7 --squash --subject "sha=${SHA}"`))).toBe(true)
  expect(denied(await bash($, `echo sha=${SHA}; gh pr merge 7`))).toBe(true)
})

test('only the comment command itself carries its verdict', async ($, on) => {
  ok(on)
  await bash($, `echo --body "gpress-review: PASS ${SHA}"; gh pr comment 7 --body "lgtm"`)
  expect(denied(await bash($, `gh pr merge 7 --match-head-commit ${SHA}`))).toBe(true)
})

test('a repository named after the number still counts', async ($, on) => {
  ok(on)
  await bash($, `gh pr comment 7 --body "gpress-review: PASS ${SHA}" --repo other/x`)
  expect(denied(await merge($, 7, SHA))).toBe(true)
})

test('a verdict that mentions bash and pushes is still posted', async ($, on) => {
  ok(on)
  const r: any = await bash($, `gh pr comment 7 --body "gpress-review: PASS ${SHA}\n- bash hooks ok; git push origin main refused"`)
  expect(denied(r)).toBe(false)
  expect(denied(await bash($, `gh pr merge 7 --match-head-commit "${SHA}"`))).toBe(false)
})

test('a heredoc body counts', async ($, on) => {
  ok(on)
  await bash($, `gh pr comment 7 --body "$(cat <<'EOF'\ngpress-review: PASS ${SHA}\n- all checks pass\nEOF\n)"`)
  expect(denied(await bash($, `gh pr merge 7 --match-head-commit ${SHA}`))).toBe(false)
})

test('a verdict posted through the REST comments route counts', async ($, on) => {
  ok(on)
  await bash($, `gh api repos/soumyadebroy3/gpress/issues/7/comments -f body="$(cat <<'EOF'\ngpress-review: PASS ${SHA}\n- all checks pass\nEOF\n)"`)
  expect(denied(await merge($, 7, SHA))).toBe(false)
  await bash($, `gh api repos/soumyadebroy3/gpress/issues/8/comments -f body="$(cat <<'EOF'\ngpress-review: FINDINGS ${SHA}\nowner to merge: an outside pull request\nEOF\n)"`)
  expect(denied(await merge($, 8, SHA))).toBe(true)
})

test('reading comments does not record a verdict', async ($, on) => {
  ok(on)
  await bash($, `gh api repos/soumyadebroy3/gpress/issues/7/comments | grep "gpress-review: PASS ${SHA}"`)
  await bash($, `gh pr comment 7 --help; echo "gpress-review: PASS ${SHA}"`)
  expect(denied(await bash($, `gh pr merge 7 --match-head-commit ${SHA}`))).toBe(true)
})

test('a failed comment records no verdict', async ($, on) => {
  on('tool.call', () => ({ result: 'boom', isError: true }))
  await comment($, 5, `gpress-review: PASS ${SHA}`)
  const r: any = await merge($, 5, SHA)
  expect(JSON.stringify(r)).toContain('no review')
})

test('other tool calls pass untouched', async ($, on) => {
  ok(on)
  expect(denied(await bash($, 'cargo fmt --all --check'))).toBe(false)
  expect(denied(await bash($, 'grep -n "gh pr merge 12" README.md'))).toBe(false)
  expect(denied(await bash($, 'git push -u origin claude/pr-review-mod'))).toBe(false)
  expect(denied(await bash($, 'gh api repos/soumyadebroy3/gpress/pulls/5/merge'))).toBe(false)
  expect(denied(await bash($, 'gh pr merge 7 --disable-auto'))).toBe(false)
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

test('/review-pr sends the protocol to the model by itself', async ($, on) => {
  const sent: string[] = []
  on('prompt.submit', (_$: any, e: any) => { sent.push(e.text); return { text: e.text } })
  const clock = mock.clock(on)
  const r: any = await $.command.run({ command: 'review-pr', args: '12' } as any)
  expect(r.text).toContain('#12')
  await clock.advance(10)
  expect(sent.join('')).toContain('Review gpress pull request #12')
  expect(sent.join('')).toContain('gpress-review: PASS <head sha>')
  expect(sent.join('')).toContain('owner to merge: an outside pull request')
  expect(sent.join('')).toContain('gpress/pulls/12 ')
  expect(sent.join('')).not.toMatch(/`gh [^`]*#12/)
})

test('the rule goes in the main prompt, not a subagent', async ($, on) => {
  on('prompt.compose', () => ({ sections: [{ id: 'base', text: 'base', scope: 'session' }] }))
  const base = { model: 'm', promptModel: 'm', surfaces: [], outputStyle: null, traits: [] }
  const main: any = await $.prompt.compose({ ...base, tools: ['Agent', 'Bash'] } as any)
  const sub: any = await $.prompt.compose({ ...base, tools: ['Bash'] } as any)
  expect(main.sections.map((s: any) => s.id)).toContain('pr-review')
  expect(sub.sections.map((s: any) => s.id)).not.toContain('pr-review')
})
