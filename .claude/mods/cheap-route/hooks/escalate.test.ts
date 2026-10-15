import { test, expect } from 'claude-code/testing'

const PARENT = 'claude-opus-5-5'

// Spawns a read-only task (routed to haiku) whose agent id is a1, then runs
// one step of it. Beneath, the cheap model answers `cheap`; any other model
// answers `smart`. Returns the models asked, in order, and the step's answer.
const run = async ($: any, on: any, cheap: { answer?: string; toolUses?: any[]; stopReason?: any }, index = 0, steps = 1) => {
  const asked: string[] = []
  on('agent.spawn', (_$: any, e: any) => ({ model: e.model ?? PARENT, agentId: 'a1' }))
  on('turn.step', async function* (_$: any, e: any) {
    asked.push(e.model)
    const base = { turnId: e.turnId, index: e.index, usage: null }
    if (e.model === 'haiku') {
      return { ...base, answer: '', toolUses: [], stopReason: 'end_turn', ...cheap }
    }
    return { ...base, answer: 'smart', toolUses: [], stopReason: 'end_turn' }
  })
  await $.agent.spawn({ description: 'task', prompt: 'List the files in docs/tasks.', parentModel: PARENT })
  let answer = ''
  for (let i = index; i < index + steps; i++) {
    const stream = $.turn.step({ turnId: 't1', index: i, model: 'haiku', messageCount: 2 + i, agentId: 'a1' })
    let step = await stream.next()
    while (!step.done) step = await stream.next()
    answer = step.value.answer
  }
  return { asked, answer }
}

test('a good cheap answer stands', async ($, on) => {
  const r = await run($, on, { answer: 'docs/tasks has 6 briefs.' })
  expect(r.asked).toEqual(['haiku'])
  expect(r.answer).toBe('docs/tasks has 6 briefs.')
})

test('an edit switches the step to the session model', async ($, on) => {
  const r = await run($, on, { toolUses: [{ name: 'Edit', input: { file_path: 'a.rs' } }], stopReason: 'tool_use' })
  expect(r.asked).toEqual(['haiku', PARENT])
  expect(r.answer).toBe('smart')
})

test('a mutating shell command switches up', async ($, on) => {
  const r = await run($, on, { toolUses: [{ name: 'Bash', input: { command: 'git commit -am wip' } }], stopReason: 'tool_use' })
  expect(r.asked).toEqual(['haiku', PARENT])
})

test('a read-only shell command stays cheap', async ($, on) => {
  const r = await run($, on, { toolUses: [{ name: 'Bash', input: { command: 'ls docs/tasks | wc -l' } }], stopReason: 'tool_use' })
  expect(r.asked).toEqual(['haiku'])
})

test('an unsure answer switches up', async ($, on) => {
  const r = await run($, on, { answer: "I'm not sure which brief covers that." })
  expect(r.asked).toEqual(['haiku', PARENT])
  expect(r.answer).toBe('smart')
})

test('an empty answer switches up', async ($, on) => {
  const r = await run($, on, { answer: '  ' })
  expect(r.asked).toEqual(['haiku', PARENT])
})

test('a refusal switches up', async ($, on) => {
  const r = await run($, on, { answer: 'no', stopReason: 'refusal' })
  expect(r.asked).toEqual(['haiku', PARENT])
})

test('a long-running task switches up', async ($, on) => {
  const r = await run($, on, { answer: 'still going', toolUses: [{ name: 'Read', input: {} }], stopReason: 'tool_use' }, 20)
  expect(r.asked).toEqual(['haiku', PARENT])
})

test('once switched up, later steps go straight to the session model', async ($, on) => {
  const r = await run($, on, { answer: '' }, 0, 2)
  expect(r.asked).toEqual(['haiku', PARENT, PARENT])
})

test('the main loop is never touched', async ($, on) => {
  const asked: string[] = []
  on('turn.step', async function* (_$: any, e: any) {
    asked.push(e.model)
    return { turnId: e.turnId, index: e.index, answer: '', toolUses: [], stopReason: 'end_turn', usage: null }
  })
  const s = $.turn.step({ turnId: 't0', index: 0, model: PARENT, messageCount: 1 } as any)
  for await (const _ of s) { /* drain */ }
  expect(asked).toEqual([PARENT])
})
