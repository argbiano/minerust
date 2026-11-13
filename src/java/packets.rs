// Packet ids for protocol 777 (26.3), from the data generator's reports/packets.json.

pub const PROTOCOL_VERSION: i32 = 777;
pub const VERSION_NAME: &str = "26.3";

pub mod status {
    pub const SB_STATUS_REQUEST: i32 = 0x00;
    pub const SB_PING_REQUEST: i32 = 0x01;
    pub const CB_STATUS_RESPONSE: i32 = 0x00;
    pub const CB_PONG_RESPONSE: i32 = 0x01;
}

pub mod login {
    pub const SB_HELLO: i32 = 0x00;
    pub const SB_LOGIN_ACKNOWLEDGED: i32 = 0x03;
    pub const CB_LOGIN_DISCONNECT: i32 = 0x00;
    pub const CB_LOGIN_FINISHED: i32 = 0x02;
    pub const CB_LOGIN_COMPRESSION: i32 = 0x03;
}

pub mod configuration {
    pub const SB_FINISH_CONFIGURATION: i32 = 0x03;
    pub const SB_SELECT_KNOWN_PACKS: i32 = 0x07;
    pub const CB_CUSTOM_PAYLOAD: i32 = 0x01;
    pub const CB_FINISH_CONFIGURATION: i32 = 0x03;
}

pub mod play {
    pub const SB_ACCEPT_TELEPORTATION: i32 = 0x00;
    pub const SB_ATTACK: i32 = 0x01;
    pub const SB_PLAYER_ACTION: i32 = 0x29;
    pub const SB_CLIENT_COMMAND: i32 = 0x0C;
    pub const SB_MOVE_PLAYER_ROT: i32 = 0x20;
    pub const SB_MOVE_PLAYER_STATUS_ONLY: i32 = 0x21;
    pub const CB_ADD_ENTITY: i32 = 0x01;
    pub const CB_BLOCK_CHANGED_ACK: i32 = 0x04;
    pub const CB_LEVEL_EVENT: i32 = 0x2F;
    pub const CB_ANIMATE: i32 = 0x02;
    pub const CB_BLOCK_UPDATE: i32 = 0x08;
    pub const CB_ENTITY_POSITION_SYNC: i32 = 0x23;
    pub const CB_HURT_ANIMATION: i32 = 0x2B;
    pub const CB_REMOVE_ENTITIES: i32 = 0x4E;
    pub const CB_RESPAWN: i32 = 0x54;
    pub const CB_ROTATE_HEAD: i32 = 0x55;
    pub const CB_SET_ENTITY_DATA: i32 = 0x65;
    pub const CB_SET_HEALTH: i32 = 0x6A;
    pub const CB_PLAYER_INFO_REMOVE: i32 = 0x46;
    pub const CB_PLAYER_INFO_UPDATE: i32 = 0x47;
    pub const CB_TAB_LIST: i32 = 0x7D;
    pub const SB_CHAT_COMMAND: i32 = 0x07;
    pub const SB_CHAT_COMMAND_SIGNED: i32 = 0x08;
    pub const SB_CHAT: i32 = 0x09;
    pub const SB_KEEP_ALIVE: i32 = 0x1C;
    pub const SB_MOVE_PLAYER_POS: i32 = 0x1E;
    pub const SB_MOVE_PLAYER_POS_ROT: i32 = 0x1F;
    pub const CB_CHUNK_BATCH_FINISHED: i32 = 0x0B;
    pub const CB_CHUNK_BATCH_START: i32 = 0x0C;
    pub const CB_COMMANDS: i32 = 0x10;
    pub const CB_DISCONNECT: i32 = 0x20;
    pub const CB_ENTITY_EVENT: i32 = 0x22;
    pub const CB_FORGET_LEVEL_CHUNK: i32 = 0x26;
    pub const CB_GAME_EVENT: i32 = 0x27;
    pub const CB_KEEP_ALIVE: i32 = 0x2D;
    pub const CB_LEVEL_CHUNK_WITH_LIGHT: i32 = 0x2E;
    pub const CB_LOGIN: i32 = 0x32;
    pub const CB_PLAYER_ABILITIES: i32 = 0x41;
    pub const CB_PLAYER_POSITION: i32 = 0x49;
    pub const CB_SET_CHUNK_CACHE_CENTER: i32 = 0x60;
    pub const CB_SET_TIME: i32 = 0x73;
    pub const CB_SYSTEM_CHAT: i32 = 0x7C;
}
