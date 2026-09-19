export const name = 'svc-tools'
export const inject = ['tools', 'systemPrompt']

const { execFile } = process.getBuiltinModule('node:child_process')
const TIMEOUT_MS = 30_000
const MAX_BUFFER = 1_000_000

const string = (description) => ({ type: 'string', description })
const integer = (description) => ({ type: 'integer', description })

const toolSpecs = [
  {
    name: 'rename',
    description: 'Rename an existing definition and update all tracked references.',
    required: ['entity', 'new_name'],
    properties: { entity: string('Definition name or entity id.'), new_name: string('New definition name.') },
  },
  {
    name: 'move',
    description: 'Move a definition beneath a different parent definition.',
    required: ['entity', 'new_parent'],
    properties: { entity: string('Definition name or entity id.'), new_parent: string('Destination parent name or id.'), ordinal: integer('Optional sibling position.') },
  },
  {
    name: 'relocate',
    description: 'Move a definition to a different tracked file and sibling position.',
    required: ['entity', 'file', 'ordinal'],
    properties: { entity: string('Definition name or entity id.'), file: string('Destination repository-relative file.'), ordinal: integer('Destination sibling position.') },
  },
  {
    name: 'extract',
    description: 'Hoist a nested definition to an optional new parent.',
    required: ['entity'],
    properties: { entity: string('Definition name or entity id.'), new_parent: string('Optional destination parent name or id.') },
  },
  {
    name: 'inline',
    description: 'Inline a definition that has a single tracked use.',
    required: ['entity'],
    properties: { entity: string('Definition name or entity id.') },
  },
  {
    name: 'add_def',
    description: 'Add one complete definition, including its signature.',
    required: ['ordinal', 'definition', 'intent'],
    properties: { id: string('Optional stable entity UUID; svc generates one when omitted.'), parent: string('Optional parent name or id.'), ordinal: integer('Sibling position.'), definition: string('Complete item, signature included.'), intent: string('Declared reason for the change.') },
  },
  {
    name: 'delete',
    description: 'Delete a definition and its unreferenced descendants.',
    required: ['entity', 'intent'],
    properties: { entity: string('Definition name or entity id.'), intent: string('Declared reason for the deletion.') },
  },
  {
    name: 'edit_def',
    description: 'Replace one complete definition, signature included. This cannot rename it; use rename instead.',
    required: ['entity', 'definition', 'intent'],
    properties: { entity: string('Definition name or entity id.'), definition: string('Complete replacement item, signature included.'), intent: string('Declared intent such as refactor, fix, feature, or docs.') },
  },
  {
    name: 'list_defs',
    description: 'List tracked definitions and their stable entity ids.',
    required: [],
    properties: {},
  },
  {
    name: 'show_def',
    description: 'Show one tracked definition and its semantic representation.',
    required: ['entity'],
    properties: { entity: string('Definition name or entity id.') },
  },
  {
    name: 'search',
    description: 'Search tracked definitions by name or source text.',
    required: ['query'],
    properties: { query: string('Name or text to find.') },
  },
  {
    name: 'diff',
    description: 'Compare two changes, snapshots, or entities.',
    required: ['a', 'b'],
    properties: { a: string('First revision or entity.'), b: string('Second revision or entity.') },
  },
  {
    name: 'blame',
    description: 'Show the evolution history responsible for a definition.',
    required: ['entity'],
    properties: { entity: string('Definition name or entity id.') },
  },
  {
    name: 'evolog',
    description: 'Show the evolution history for a stable change id.',
    required: ['change'],
    properties: { change: string('Change id or unique prefix.') },
  },
  {
    name: 'classify',
    description: 'Predict the observed class of a complete replacement definition without changing the repository.',
    required: ['entity', 'definition'],
    properties: { entity: string('Definition name or entity id.'), definition: string('Complete proposed item, signature included.') },
  },
]

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function validateArgs(spec, args) {
  if (!isObject(args)) throw new Error(`${spec.name} arguments must be a JSON object`)
  for (const key of spec.required) {
    if (!(key in args)) throw new Error(`${spec.name} requires ${key}`)
  }
  for (const [key, value] of Object.entries(args)) {
    const property = spec.properties[key]
    if (!property) throw new Error(`${spec.name} does not accept ${key}`)
    if (property.type === 'integer' && !Number.isInteger(value)) throw new Error(`${key} must be an integer`)
    if (property.type === 'string' && typeof value !== 'string') throw new Error(`${key} must be a string`)
  }
}

function cliArgs(spec, args) {
  const result = [spec.name.replaceAll('_', '-')]
  for (const [key, value] of Object.entries(args)) {
    result.push(`--${key.replaceAll('_', '-')}`, String(value))
  }
  result.push('--json')
  return result
}

function runSvc(spec, args, signal, config) {
  const bin = config.svcBin ?? process.env.SVC_BIN
  if (typeof bin !== 'string' || !bin.startsWith('/')) {
    throw new Error('SVC_BIN must be the absolute path to the svc binary')
  }
  return new Promise((resolve, reject) => {
    execFile(bin, cliArgs(spec, args), { signal, timeout: TIMEOUT_MS, maxBuffer: MAX_BUFFER }, (error, stdout, stderr) => {
      if (error) {
        reject(new Error(stderr.trim() || error.message || `svc ${spec.name} failed`))
        return
      }
      try {
        resolve(JSON.parse(stdout))
      } catch (parseError) {
        reject(new Error(`svc ${spec.name} returned invalid JSON: ${parseError.message}`))
      }
    })
  })
}

function output() {
  return {
    schema: { type: 'object' },
    render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
  }
}

export function apply(ctx, config = {}) {
  for (const spec of toolSpecs) {
    ctx.tools.register({
      name: spec.name,
      description: spec.description,
      parameters: {
        type: 'object',
        additionalProperties: false,
        required: spec.required,
        properties: spec.properties,
      },
      output: output(),
      timeoutMs: TIMEOUT_MS,
      async execute(args, exec) {
        validateArgs(spec, args)
        return runSvc(spec, args, exec.signal, config)
      },
    })
  }

  ctx.tools.register({
    name: 'list_tools',
    description: 'List the tools available to this agent. Use this to verify that direct file writes are unavailable.',
    parameters: { type: 'object', additionalProperties: false, required: [], properties: {} },
    output: output(),
    timeoutMs: TIMEOUT_MS,
    async execute(args, exec) {
      if (!isObject(args)) throw new Error('list_tools arguments must be a JSON object')
      return { tools: ctx.tools.schemas(exec.agent).map((schema) => schema.name) }
    },
  })

  ctx.on('tools/pre-execute', (exec, next) => {
    if (exec.name !== 'edit_def') return next()
    const spec = toolSpecs.find((candidate) => candidate.name === 'edit_def')
    try {
      validateArgs(spec, exec.arguments)
    } catch (error) {
      return { kind: 'deny', reason: error.message }
    }
    return { kind: 'ask', reason: 'edit_def replaces a complete definition' }
  })

  ctx.on('agent/created', ({ agent }) => {
    agent.ctx.tools.restrict({ deny: ['edit', 'write'] })
  })

  ctx.systemPrompt.section({
    name: 'ops:policy',
    order: 50,
    text: 'Use svc semantic operations for every code change. Prefer rename, move, relocate, extract, inline, add_def, or delete over edit_def. edit_def must contain the complete item, signature included, and cannot rename it. Direct file editing and shell commands are unavailable. Read, search, and inspect freely before changing code.',
  })
}
