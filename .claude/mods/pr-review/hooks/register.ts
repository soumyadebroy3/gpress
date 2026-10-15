import type { Register } from 'claude-code'
import { mergeBlocked, mergeTarget, postedVerdict, type Verdict } from './gate'
import { protocol, SYSTEM_SECTION } from './protocol'

export const register: Register = (on, options) => {
  const requireReview = options.requireReview !== false
  // The last verdict this session posted on each pull request.
  const verdicts = new Map<string, Verdict>()
  // A review asked for by /review-pr, sent once the command's turn ends.
  let queued: string | undefined

  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'review-pr',
      description: 'Review a gpress pull request with fresh agents: /review-pr <number>',
    })
    return next(e)
  })

  on('command.run', { command: 'review-pr' }, ($, e) => {
    const pr = /\d+/.exec(e.args)?.[0]
    if (!pr) return { text: 'Usage: /review-pr <pull request number>' }
    queued = protocol(`#${pr}`)
    return { text: `Reviewing pull request #${pr} with fresh agents.` }
  })

  on('turn.complete', async ($, e, next) => {
    const done = await next(e)
    if (e.agentId === undefined && queued) {
      const text = queued
      queued = undefined
      void $.prompt.submit({ text })
    }
    return done
  })

  // The main loop's prompt carries the rule; subagents (no Agent tool) don't.
  on('prompt.compose', async ($, e, next) => {
    const composed = await next(e)
    if (!e.tools.includes('Agent')) return composed
    return { sections: [...composed.sections, { id: 'pr-review', text: SYSTEM_SECTION, scope: 'session' as const }] }
  })

  on('tool.call', async ($, e, next) => {
    const input = e as unknown as Record<string, unknown>
    const target = requireReview ? mergeTarget(e.tool, input) : undefined
    if (target) {
      const why = mergeBlocked(target, verdicts.get(target.pr))
      if (why) return { deny: `pr-review: ${why}` }
    }
    const result = await next(e)
    const posted = result.deny === undefined && result.isError !== true ? postedVerdict(e.tool, input) : undefined
    if (posted) {
      verdicts.set(posted.pr, posted.verdict)
      $.ui.status(`pr-review: #${posted.pr} ${posted.verdict.pass ? 'PASS' : 'FINDINGS'} ${posted.verdict.sha.slice(0, 7)}`)
    }
    return result
  }).catch(($, e, next) =>
    next.called || !mergeTarget(e.tool, e as unknown as Record<string, unknown>)
      ? next(e)
      : { deny: 'pr-review: its merge check failed; try again.' },
  )
}
