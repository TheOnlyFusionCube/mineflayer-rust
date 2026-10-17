// NOTE: This file is @generated automatically by codegen/packet.py.
// Don't edit it directly!

use mineflayer_protocol_macros::declare_state_packets;

declare_state_packets!(HandshakePacket,
    Clientbound => [
    ],
    Serverbound => [
        intention,
    ]
);
