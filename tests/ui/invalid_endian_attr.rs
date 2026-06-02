use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[endian(native)]
    #[derive(Clone)]
    struct Bad {
        value: u16,
    }
}

fn main() {}
