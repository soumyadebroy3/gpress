import { test, expect } from 'claude-code/testing'

const spawnWith = async ($: any, on: any, args: { prompt: string; subagentType?: string; model?: string }) => {
  let seen: string | undefined
  on('agent.spawn', (_$: any, e: any) => {
    seen = e.model
    return { model: e.model ?? 'session-model' }
  })
  await $.agent.spawn({ description: 'task', ...args })
  return seen
}

test('a lookup goes to haiku', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Find every file that imports qpdf and list their paths.' })).toBe('haiku')
})

test('a summary goes to haiku', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Summarize the open PRs on soumyadebroy3/gpress in five lines.' })).toBe('haiku')
})

test('Explore searches go to haiku', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Where do we call the flate encoder setup?', subagentType: 'Explore' })).toBe('haiku')
})

test('implementation work stays on the session model', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Implement the effort-0 fast tier and benchmark it against qpdf.' })).toBeUndefined()
})

test('a bug fix stays even when it starts with find', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Find and fix the bug that makes the object stream writer crash.' })).toBeUndefined()
})

test('a rename stays on the session model', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Rename parse_xref to read_xref across the crate.' })).toBeUndefined()
})

test('a changelog edit stays on the session model', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Update the changelog with the fast-tier numbers.' })).toBeUndefined()
})

test('a lookup that ends in a commit stays on the session model', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'List the stale bench files and delete them, then commit.' })).toBeUndefined()
})

test('an unclear task stays on the session model', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Handle the qpdf comparison for the sample120 set.' })).toBeUndefined()
})

test('an explicit model choice is kept', async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'List the files in docs/tasks.', model: 'opus' })).toBe('opus')
})

test('sonnet can be the cheap model', { options: { cheapModel: 'sonnet' } }, async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'Check the status of CI on the fast-tier branch.' })).toBe('sonnet')
})

test('off means no routing', { options: { enabled: false } }, async ($, on) => {
  expect(await spawnWith($, on, { prompt: 'List the files in docs/tasks.' })).toBeUndefined()
})

test('the command reports what was routed', async ($, on) => {
  await spawnWith($, on, { prompt: 'List the files in docs/tasks.' })
  const out = await $.command.run({ command: 'cheap-route', args: '' } as any)
  expect(JSON.stringify(out)).toContain('1 read-only task(s) sent to haiku')
})
