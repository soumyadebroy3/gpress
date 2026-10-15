import type { Register } from 'claude-code'
import { classify } from './classify'

type Decision = { at: string; task: string; model: string; reason: string }

export const register: Register = (on, options) => {
  const enabled = options.enabled !== false
  const cheapModel = String(options.cheapModel ?? 'haiku')
  const maxEasyChars = Number(options.maxEasyChars ?? 2000)
  const log: Decision[] = []
  let routed = 0
  let kept = 0

  const note = (d: Decision) => {
    log.push(d)
    if (log.length > 50) log.shift()
  }

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'cheap-route',
      description: 'Show which subagent tasks were sent to the cheaper model',
    })
    return next(e)
  })

  on('command.run', { command: 'cheap-route' }, () => {
    const head = enabled
      ? `cheap-route: ${routed} easy task(s) sent to ${cheapModel}, ${kept} kept on the session model.`
      : 'cheap-route is off (turn it on in /config).'
    const rows = log
      .slice(-15)
      .map(d => `- ${d.model.padEnd(8)} ${d.task}  (${d.reason})`)
    return { text: [head, ...rows].join('\n') }
  })

  on('agent.spawn', ($, e, next) => {
    // Leave alone: routing off, an explicit model choice, forks and resumes
    // (both keep their model), and a parent already on the cheap model.
    if (!enabled || e.model || e.fork || e.resume || (e.parentModel ?? '').includes(cheapModel)) {
      return next(e)
    }
    const v = classify(e.prompt, e.subagentType, maxEasyChars)
    const task = (e.description || e.prompt).replace(/\s+/g, ' ').slice(0, 60)
    const at = new Date().toISOString()
    if (!v.easy) {
      kept++
      note({ at, task, model: 'session', reason: v.reason })
      return next(e)
    }
    routed++
    note({ at, task, model: cheapModel, reason: v.reason })
    $.ui.status(`cheap-route: ${routed} easy task(s) → ${cheapModel}`)
    return next({ ...e, model: cheapModel })
  })
}
