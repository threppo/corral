# Corral settings kit

This kit implements the approved settings design primitives. It owns no settings file,
window handle, desktop state or file operation. Pages continue to live-apply through their existing
`Field`/`Hit` adapters.

* `theme::Theme::new(light)` supplies the complete semantic palette. `Type::style(dpi, color)`
  supplies the type ramp and detects Segoe UI Variable, falling back to Segoe UI.
* `controls::draw_control(ctx, kind, bounds, label, state, theme, dpi, fraction)` draws a primitive.
  Bounds are **DIPs**, converted to snapped device pixels exactly once. `fraction` is the unrounded
  slider thumb position. `VisualState::focused` means keyboard-visible focus, not mouse focus.
  `draw_segmented`, `draw_swatch` and `draw_dividers` draw composed segment tracks, colored swatches
  and inset card dividers. Draw row text and its trailing control separately, so descriptions wrap
  and row hit bounds remain independent of the smaller trailing control. `draw_control`'s composed
  kinds are background/text primitives; they do not invent descriptions, destinations or tooltips.
* `Control::StepSlider` is the 240 x 32-DIP five-tick variant; `Control::Slider` remains
  240 x 24. `draw_slider_model` selects the variant and consumes `SliderModel::fraction`, so thumbs
  remain smooth during a drag and snap when the model releases. `slider_ticks` exposes the snapped tick rectangles.
* `controls::stepper_parts` shares the painted 32-DIP minus/plus and 40-DIP value hit geometry.
  Stepper hits are always 0=minus, 1=value, 2=plus. Runtime +/- use the existing spacing save/reload
  path; the value remains a direct-entry target and the widget is one keyboard focus stop.
* `controls::hit_control` receives a DIP bounds and a **device pixel** pointer. Supply the whole
  row bounds for toggle/navigate/radio/check row interaction. Supply actual control bounds for
  sliders, dropdowns and buttons. Hidden controls are omitted by the page; disabled controls return
  no hit. Compound controls can return a part index; Stepper uses its fixed end buttons instead of
  equal divisions. `segmented_width` accepts label widths already measured in DIPs and ensures no segment is narrower than its text plus padding.
* `layout::WindowLayout` caps content at 760 DIPs and never accepts a nav scroll offset.
  `setting_row` stacks below 520 DIPs. The caller measures wrapped text and supplies its height.
  `anchor_scroll` preserves an unchanged anchor across a gate-induced layout change.
  `expand_scroll` reveals only the first expander row where possible without hiding its header.
  `reveal_focus` minimally reveals a focused bounds with 16 DIP margin; masters reset to the top.
* `focus::FocusModel` traverses one nav stop, then master, then page controls. Pass an enabled mask
  in reading order; hidden/inert entries are false. Reconcile after gates change. `mouse()` hides
  focus rings; `key()` shows them. `keyboard_action` returns a pure action to translate through the
  same existing setting application path used by pointer input. `switch_page` wraps Ctrl+Tab.
* `slider::SliderModel` keeps a floating thumb fraction independently of the rounded stored value.
  Steps retain the unmodified loaded value until interaction, report nearest step while dragging,
  and snap the thumb on release. `step` handles percent or named-step keyboard movement.
* `menu::PopupGeometry::new` accepts anchor/work rectangles in **screen device pixels**, then the
  **destination monitor's DPI**, width in DIPs and item count. The returned popup rectangle and
  surface dimensions are device pixels. `row`/`hit` use popup-local device pixels. Recompute on
  WM_DPICHANGED and create a fresh surface; never stretch an owner's surface. `MenuState` handles
  keyboard selection and type-ahead. The window shell owns focus, popup creation and DWM shadow.

Kit unit tests cover 96/120/144/240 DPI geometry, every primitive's hit/disabled rules,
focus traversal, menu geometry, small work areas, scroll maths, step snapping and fraction/value
decoupling. The offscreen test writes state sheets and software render timing samples under
`target/qa/settings-redesign/u1/`. These are rendered evidence, not live window verification.
