// Differential check of the presence codec against node-minecraft-protocol.
// Reads vectors (JSON lines from `examples/vectors`) on stdin. For each one it
// parses the bytes with node-minecraft-protocol's interpreter for the same
// protocol, state and direction, requires every byte to be consumed and the
// packet name to match, then re-encodes the parsed packet and requires the
// same bytes. Prints a summary and exits 1 on any mismatch.
//
// npm install minecraft-protocol@1.68.0 minecraft-data@3.117.0
// cargo run --release -p mineflayer-presence --example vectors -- 1000 | node scripts/presence-diff/check.js
'use strict'
const readline = require('readline')
const { ProtoDefCompiler } = require('protodef').Compiler
const nbt = require('prismarine-nbt')
const minecraftData = require('minecraft-data')

// node-minecraft-protocol 1.68.0 writes an inline registryEntryHolder by
// skipping the leading byte (`offset += 1`, compiler-minecraft.js:114)
// instead of writing varint 0, so that byte is whatever the buffer held.
// Vanilla (ByteBufCodecs.holder) writes 0. This writer is nmp's own with that
// one line fixed; its reader is untouched.
const HOLDER_WRITE_FIX = {
  Read: {},
  SizeOf: {},
  Write: {
    registryEntryHolder: ['parametrizable', (compiler, opts) => {
      const baseName = `value.${opts.baseName}`
      const otherwiseName = `value.${opts.otherwise.name}`
      return compiler.wrapCode(`
if (${baseName} != null) {
  offset = ${compiler.callType(`${baseName} + 1`, 'varint')}
} else if (${otherwiseName}) {
  offset = ${compiler.callType('0', 'varint')}
  offset = ${compiler.callType(`${otherwiseName}`, opts.otherwise.type)}
} else {
  throw new Error('registryEntryHolder needs ${baseName} or ${otherwiseName}')
}
return offset
      `.trim())
    }]
  }
}

// The same compile node-minecraft-protocol's serializer does, without the
// version cache that would key on its own minecraft-data lookups.
const protos = {}
function proto (version, state, direction) {
  const key = `${version};${state};${direction}`
  if (!protos[key]) {
    const mcData = minecraftData(version)
    if (!mcData) throw new Error(`no minecraft-data for ${version}`)
    const compiler = new ProtoDefCompiler()
    compiler.addTypes(require('minecraft-protocol/src/datatypes/compiler-minecraft'))
    compiler.addProtocol(mcData.protocol, [state, direction])
    nbt.addTypesToCompiler('big', compiler)
    compiler.addTypes(HOLDER_WRITE_FIX)
    protos[key] = compiler.compileProtoDefSync()
  }
  return protos[key]
}

const stats = { vectors: 0, ok: 0, fail: 0, skipped: 0, byKey: {} }
const skippedVersions = new Set()
const failures = []
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
rl.on('line', (line) => {
  if (!line) return
  const v = JSON.parse(line)
  stats.vectors++
  const key = `${v.protocol} ${v.state} ${v.direction} ${v.name}`
  // releases newer than this minecraft-data (derived rows) have no reference
  if (!minecraftData(v.version)) {
    stats.skipped++
    skippedVersions.add(v.version)
    return
  }
  const buf = Buffer.from(v.hex, 'hex')
  let why = null
  try {
    const p = proto(v.version, v.state, v.direction)
    const parsed = p.parsePacketBuffer('packet', buf)
    if (parsed.metadata.size !== buf.length) why = `consumed ${parsed.metadata.size} of ${buf.length}`
    else if (parsed.data.name !== v.name) why = `name ${parsed.data.name}`
    else {
      const again = p.createPacketBuffer('packet', parsed.data)
      if (!again.equals(buf)) why = `re-encoded ${again.toString('hex')} != ${v.hex}`
    }
  } catch (e) {
    why = `error ${String(e.message || e).slice(0, 200)}`
  }
  if (why) {
    stats.fail++
    stats.byKey[key] = (stats.byKey[key] || 0) + 1
    if (failures.length < 40) failures.push({ key, why, hex: v.hex.slice(0, 120), value: JSON.stringify(v.value).slice(0, 200) })
  } else stats.ok++
})
rl.on('close', () => {
  for (const f of failures) console.log('FAIL', f.key, '|', f.why, '|', f.value)
  const keys = Object.keys(stats.byKey)
  console.log(`vectors=${stats.vectors} ok=${stats.ok} fail=${stats.fail} failing_packets=${keys.length} skipped=${stats.skipped} (no reference data: ${[...skippedVersions].join(' ') || 'none'})`)
  for (const k of keys.slice(0, 60)) console.log('  ', k, stats.byKey[k])
  process.exit(stats.fail ? 1 : 0)
})
