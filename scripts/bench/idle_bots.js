// Resource probe for upstream mineflayer, the twin of
// examples/idle_bots.rs: connect <count> offline-mode bots one after
// another (each spawns, then 100 ms pass),
// keep them idle for <seconds> after the last spawn, then quit. Run it
// under /usr/bin/time with mineflayer installed on NODE_PATH:
//
//   NODE_PATH=<dir>/node_modules /usr/bin/time -l \
//     node scripts/bench/idle_bots.js localhost 25566 10 60
const mineflayer = require('mineflayer')
const { once } = require('events')

async function main () {
  const [host, port, count, seconds] = process.argv.slice(2)
  if (!seconds) {
    console.log('Usage: node idle_bots.js <host> <port> <count> <seconds>')
    process.exit(1)
  }
  const start = Date.now()
  const bots = []
  for (let i = 0; i < Number(count); i++) {
    const bot = mineflayer.createBot({
      host, port: Number(port), username: `idle${i}`, viewDistance: 4
    })
    await once(bot, 'spawn')
    bots.push(bot)
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  console.log(`${count} bots spawned in ${((Date.now() - start) / 1000).toFixed(1)} s`)
  await new Promise(resolve => setTimeout(resolve, Number(seconds) * 1000))
  bots.forEach(bot => bot.quit())
  console.log(`idle for ${seconds} s`)
  setTimeout(() => process.exit(0), 1000)
}

main()
