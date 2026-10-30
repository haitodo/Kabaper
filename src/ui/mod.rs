pub mod layout_calc;
pub mod render;
pub mod state;
pub mod types;
pub mod window;

pub use layout_calc::*;
pub use render::*;
pub use state::*;
pub use types::*;
pub use window::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filmstrip_layout() {
        let (strip_y, strip_h, item_w, start, end) =
            get_filmstrip_layout(1000, 800, 5, 20, 96);
        assert_eq!(strip_h, 92);
        assert_eq!(strip_y, 800 - 92);
        assert!(item_w > 0);
        assert!(start <= 5);
        assert!(end > 5);
        assert!(end <= 20);
    }
}
