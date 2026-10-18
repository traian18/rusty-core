//! Module docs.
use std::collections::HashMap;

/// Adds.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn classify(n: i32, flag: bool) -> &'static str {
    // comment line
    if n < 0 && flag {
        "negative"
    } else if n == 0 || !flag {
        "zero"
    } else {
        for i in 0..n {
            if i > 10 {
                return "big";
            }
        }
        "positive"
    }
}

pub fn kind(v: Option<u8>) -> u8 {
    match v {
        Some(0) => 0,
        Some(x) if x > 5 => 1,
        _ => 2,
    }
}

pub struct Counter {
    count: u32,
    names: HashMap<String, u32>,
}

impl Counter {
    pub fn new() -> Self {
        Self { count: 0, names: HashMap::new() }
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }

    pub fn total(&self) -> u32 {
        self.count + self.size()
    }

    fn size(&self) -> u32 {
        0
    }

    pub fn name_of(&self, key: &str) -> Option<u32> {
        let found = self.names.get(key).copied();
        found.map(|v| v + 1)
    }
}
