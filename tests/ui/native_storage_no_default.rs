use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[derive(Clone)]
    struct Bad {
        value: u16,
    }
}

fn main() {}
