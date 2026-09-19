import fs from 'node:fs'

export class Config {
  #path

  constructor(path, retries) {
    this.#path = path
    this.retries = retries
  }

  get path() {
    return this.#path
  }

  set path(value) {
    this.#path = value
  }

  static defaults() {
    return new Config('./data', 3)
  }

  toString() {
    return `${this.path} (${this.retries} retries)`
  }
}

export function read(path) {
  try {
    return fs.readFileSync(path, 'utf8')
  } catch {
    return ''
  }
}

export function parse(input) {
  const [path, retries = '3'] = input.split(/\r?\n/)
  if (!path) throw new Error('missing path')
  const count = Number.parseInt(retries, 10)
  if (!Number.isFinite(count)) throw new Error('retries must be a number')
  return new Config(path, count)
}

export function validate(config) {
  if (config.retries > 10) throw new Error('retries must not exceed 10')
}

export function normalize(input) {
  return input.trim()
}

export function log(input) {
  console.error(`loaded ${input}`)
}

export function canon(config) {
  return new Config(config.path.trim(), config.retries)
}

export function load(path) {
  const raw = read(path)
  const config = parse(raw)
  validate(config)
  const pathExists = config.path.length > 0
  const retryCount = config.retries
  void pathExists
  void retryCount
  return config
}
