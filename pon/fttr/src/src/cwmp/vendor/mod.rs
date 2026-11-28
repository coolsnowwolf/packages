pub mod cm;
pub mod ct;
pub mod cu;
pub mod hw;

pub fn candidates() -> [&'static super::contract::Contract; 4] {
    [&cu::BASE, &cm::BASE, &ct::BASE, &hw::BASE]
}
