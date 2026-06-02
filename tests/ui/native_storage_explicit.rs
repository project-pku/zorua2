use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[derive(Clone)]
    struct Bad {
        value: u16 as u16,
    }
}

fn main() {}
