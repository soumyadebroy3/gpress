import type { Register, TurnStepChunk } from 'claude-code'
import { classify } from './classify'
import { outgrown } from './escalate'

type Decision = { task: string; model: string; reason: string }
type Routed = { task: string; parentModel: string; escalated?: string; errors: number }

export const register: Register = (on, options) => {
  const enabled = options.enabled !== false
  const cheapModel = String(options.cheapModel ?? 'haiku')
  const maxEasyChars = Number(options.maxEasyChars ?? 2000)
  const maxCheapSteps = Number(options.maxCheapSteps ?? 20)
  const log: Decision[] = []
  // Subagents running on the cheap model, by agent id.
  const routed = new Map<string, Routed>()
  let sent = 0
  let kept = 0
  let escalated = 0

  const note = (d: Decision) => {
    log.push(d)
    if (log.length > 50) log.shift()
  }

  const statusLine = () => `cheap-route: ${sent} → ${cheapModel}, ${escalated} switched up`

  const escalate = (r: Routed, reason: string) => {
    r.escalated = reason
    escalated++
    note({ task: r.task, model: 'session', reason: `switched up: ${reason}` })
  }

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'cheap-route',
      description: 'Show which subagent tasks ran on the cheaper model',
    })
    return next(e)
  })

  on('command.run', { command: 'cheap-route' }, () => {
    const head = enabled
      ? `cheap-route: ${sent} read-only task(s) sent to ${cheapModel}, ${escalated} switched up to the session model, ${kept} kept on the session model.`
      : 'cheap-route is off (turn it on in /config).'
    const rows = log.slice(-15).map(d => `- ${d.model.padEnd(8)} ${d.task}  (${d.reason})`)
    return { text: [head, ...rows].join('\n') }
  })

  on('agent.spawn', async ($, e, next) => {
    // Leave alone: routing off, an explicit model choice, forks and resumes
    // (both keep their model), and a parent already on the cheap model.
    if (!enabled || e.model || e.fork || e.resume || (e.parentModel ?? '').includes(cheapModel)) {
      return next(e)
    }
    const v = classify(e.prompt, e.subagentType, maxEasyChars)
    const task = (e.description || e.prompt).replace(/\s+/g, ' ').slice(0, 60)
    if (!v.easy) {
      kept++
      note({ task, model: 'session', reason: v.reason })
      return next(e)
    }
    sent++
    note({ task, model: cheapModel, reason: v.reason })
    $.ui.status(statusLine())
    const started = await next({ ...e, model: cheapModel })
    if (started.agentId) {
      routed.set(started.agentId, { task, parentModel: e.parentModel, errors: 0 })
      if (routed.size > 200) routed.delete(routed.keys().next().value!)
    }
    return started
  })

  // Each model request of a cheap subagent is held until it is whole. If it
  // shows the task has outgrown the cheap model, it is thrown away and the
  // same request goes to the session's model, which keeps the rest of the task.
  on('turn.step', async function* ($, e, next) {
    const r = e.agentId === undefined ? undefined : routed.get(e.agentId)
    if (!r) return yield* next(e)
    if (r.escalated) return yield* next({ ...e, model: r.parentModel })

    const stream = next(e)
    const held: TurnStepChunk[] = []
    let step = await stream.next()
    while (!step.done) {
      held.push(step.value)
      step = await stream.next()
    }
    const result = step.value
    const reason = outgrown(result, e.index, maxCheapSteps)
    if (!reason) {
      for (const chunk of held) yield chunk
      return result
    }
    escalate(r, reason)
    $.ui.status(statusLine())
    return yield* next({ ...e, model: r.parentModel })
  })

  // Two failed tool calls in a cheap subagent also hand it to the session model.
  on('tool.call', async ($, e, next) => {
    const result = await next(e)
    const r = e.agentId === undefined ? undefined : routed.get(e.agentId)
    if (r && !r.escalated && result.deny === undefined && result.isError === true && ++r.errors >= 2) {
      escalate(r, 'tool calls kept failing')
      $.ui.status(statusLine())
    }
    return result
  })
}
