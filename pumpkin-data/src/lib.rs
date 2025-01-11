pub mod packet {
    include!(concat!(env!("OUT_DIR"), "/packet.rs"));
}

pub mod screen {
    include!(concat!(env!("OUT_DIR"), "/screen.rs"));
}

pub mod particle {
    include!(concat!(env!("OUT_DIR"), "/particle.rs"));
}

pub mod sound {
    include!(concat!(env!("OUT_DIR"), "/sound.rs"));
}

pub mod chunk_status {
    include!(concat!(env!("OUT_DIR"), "/chunk_status.rs"));
}

pub mod game_event {
    include!(concat!(env!("OUT_DIR"), "/game_event.rs"));
}

pub mod sound_category {
    include!(concat!(env!("OUT_DIR"), "/sound_category.rs"));
}

pub mod entity_pose {
    include!(concat!(env!("OUT_DIR"), "/entity_pose.rs"));
}
