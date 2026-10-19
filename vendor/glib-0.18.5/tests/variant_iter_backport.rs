use glib::{prelude::*, Variant};

// These exercise the real GLib FFI boundary in optimized builds. Undefined
// behavior need not fail deterministically before the upstream pointer fix;
// these are compatibility checks, not a claim of a reliable UB reproducer.
#[test]
fn borrowed_strings_survive_mixed_direction_iteration() {
    let values = ["", "été", "東京", "🦀", "last"];
    let variant = values.to_variant();
    let mut iter = variant.array_iter_str().unwrap();

    let first = iter.next().unwrap();
    let last = iter.next_back().unwrap();
    assert_eq!(iter.len(), 3);
    assert_eq!(iter.nth(1), Some("東京"));
    assert_eq!(iter.next_back(), Some("🦀"));
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
    assert_eq!((first, last), ("", "last"));
}

#[test]
fn empty_and_single_element_arrays_are_fused() {
    let empty = Vec::<String>::new().to_variant();
    let mut iter = empty.array_iter_str().unwrap();
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
    assert_eq!(iter.len(), 0);

    let single = ["one"].to_variant();
    let mut iter = single.array_iter_str().unwrap();
    assert_eq!(iter.next_back(), Some("one"));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
}

#[test]
fn non_string_array_is_rejected() {
    let numbers = Variant::array_from_iter::<i32>([1i32.to_variant(), 2i32.to_variant()]);
    assert!(numbers.array_iter_str().is_err());
}
