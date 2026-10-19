Packet ids of releases minecraft-data has no data for yet, taken from the
vanilla server's own report:

```sh
java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
# generated/reports/packets.json
```

Each file maps state, direction, and Mojang packet name to the packet id.
`26.1.json` is the bridge: minecraft-data's 26.1 ids name each Mojang packet,
so `scripts/gen-presence-protocol.py` can carry every presence packet to the
later releases by name.
