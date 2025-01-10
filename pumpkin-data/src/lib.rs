pub mod packet {
    include!(concat!(env!("OUT_DIR"), "/packet.rs"));
}

pub mod screen {
    include!(concat!(env!("OUT_DIR"), "/screen.rs"));
}

pub mod particle {
    include!(concat!(env!("OUT_DIR"), "/particle.rs"));
}
