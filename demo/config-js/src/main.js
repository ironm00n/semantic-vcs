import { load } from './config.js'

const path = process.argv[2] ?? 'config.txt'

try {
  console.log(load(path).toString())
} catch (error) {
  console.error(`error: ${error.message}`)
  process.exitCode = 1
}
