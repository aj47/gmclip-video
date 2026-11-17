#![forbid(unsafe_code)]

pub fn two() -> i32 {
    2
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_test_fails() {
        assert_eq!(super::two(), 3);
    }
}
