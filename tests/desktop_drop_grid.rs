#[path = "../src/app/drop_grid.rs"]
mod grid;

#[test]
fn snaps_to_nearest_free_cell_instead_of_preserving_an_overlapping_origin() {
    let area = (0, 0, 300, 300);
    assert_eq!(grid::nearest((117, 89), (100,100), (100,100), area, (0,0), &[]), Some((100,100)));
    assert_eq!(grid::nearest((117, 89), (100,100), (100,100), area, (0,0), &[(100,100,200,200)]), Some((200,100)));
}

#[test]
fn edges_full_grid_and_group_obstacles_never_stack_or_escape() {
    let area = (0,0,200,200);
    assert_eq!(grid::nearest((999,999), (100,100), (100,100), area, (0,0), &[]), Some((100,100)));
    assert_eq!(grid::nearest((-99,-99), (100,100), (100,100), area, (0,0), &[]), Some((0,0)));
    assert_eq!(grid::nearest((0,0), (100,100), (100,100), area, (0,0), &[area]), None);
    assert_eq!(grid::nearest((0,0), (100,100), (300,100), area, (0,0), &[]), None);
    assert_eq!(grid::nearest((0,0), (0,100), (100,100), area, (0,0), &[]), None);
}

#[test]
fn mixed_dpi_negative_monitor_coordinates_and_explorer_phase() {
    let pitch = grid::scaled_spacing((100,104), 96, 240);
    assert_eq!(pitch, (250,260));
    assert_eq!(grid::scaled_spacing(pitch, 240, 96), (100,104));
    let area = (-2500,-100,0,1460);
    let anchor = grid::anchor(area, pitch, Some((-2487,-80)));
    assert_eq!(anchor, (-2487,-80));
    let first = grid::nearest((-2240,190), pitch, pitch, area, anchor, &[]).unwrap();
    assert_eq!(first, (-2237,180));
    let blocked = [(first.0,first.1,first.0+pitch.0,first.1+pitch.1)];
    let next = grid::nearest(first, pitch, pitch, area, anchor, &blocked).unwrap();
    assert_ne!(first, next);
    assert_eq!((next.0-anchor.0).rem_euclid(pitch.0), 0);
    assert_eq!((next.1-anchor.1).rem_euclid(pitch.1), 0);
}

#[test]
fn multi_item_reservations_and_larger_renderer_footprint() {
    let mut occupied = vec![];
    for _ in 0..4 {
        let p = grid::nearest((0,0), (50,50), (100,100), (0,0,200,200), (0,0), &occupied).unwrap();
        occupied.push((p.0,p.1,p.0+100,p.1+100));
    }
    assert_eq!(grid::nearest((0,0), (50,50), (100,100), (0,0,200,200), (0,0), &occupied), None);
}
