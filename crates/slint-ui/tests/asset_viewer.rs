//! The asset viewer's panels, headless: real pointer events into
//! `AssetViewerUi` pick the item under the pointer, also after a wheel scroll.
use std::cell::RefCell;
use std::rc::Rc;

use slint_ui::slint::platform::{PointerEventButton, WindowEvent};
use slint_ui::slint::{LogicalPosition, ModelRc, SharedString, VecModel};
use slint_ui::{AssetViewerUi, Overlay};

const WIDTH: u32 = 1600;
const HEIGHT: u32 = 900;
/// List-box items are 28 px tall; the first item's middle in either panel.
const ITEM: f32 = 28.0;
const FIRST_Y: f32 = 66.0;
/// Inside the left (models) and right (clips) panels.
const MODELS_X: f32 = 150.0;
const CLIPS_X: f32 = WIDTH as f32 - 150.0;

fn strings(n: usize, prefix: &str) -> ModelRc<SharedString> {
    let items: Vec<SharedString> = (0..n).map(|i| format!("{prefix}{i}").into()).collect();
    ModelRc::new(VecModel::from(items))
}

fn click(overlay: &mut Overlay<AssetViewerUi>, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    overlay.dispatch(WindowEvent::PointerMoved { position });
    overlay.dispatch(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
    overlay.dispatch(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
    overlay.render();
}

#[test]
#[ignore = "slow: >100ms — run via just test-full"]
fn clicks_pick_the_item_under_the_pointer_and_scrolling_moves_the_list() {
    let mut overlay = Overlay::with(AssetViewerUi::new).expect("viewer UI");
    overlay.resize(WIDTH, HEIGHT, 1.0);
    let picked = Rc::new(RefCell::new(Vec::new()));
    let p = picked.clone();
    overlay.ui().on_pick_model(move |i| p.borrow_mut().push(("model", i)));
    let p = picked.clone();
    overlay.ui().on_pick_clip(move |i| p.borrow_mut().push(("clip", i)));
    overlay.ui().set_models(strings(100, "model "));
    overlay.ui().set_clips(strings(5, "clip "));
    overlay.render();

    click(&mut overlay, MODELS_X, FIRST_Y + ITEM);
    click(&mut overlay, CLIPS_X, FIRST_Y + 2.0 * ITEM);
    assert_eq!(*picked.borrow(), [("model", 1), ("clip", 2)]);

    // Ten items down: the first visible row is now model 10.
    let position = LogicalPosition::new(MODELS_X, FIRST_Y);
    overlay.dispatch(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -10.0 * ITEM });
    overlay.render();
    picked.borrow_mut().clear();
    click(&mut overlay, MODELS_X, FIRST_Y);
    assert_eq!(*picked.borrow(), [("model", 10)]);

    // Pixels were drawn over the panels and not between them.
    assert!(overlay.opaque_at(MODELS_X as u32, FIRST_Y as u32));
    assert!(!overlay.opaque_at(WIDTH / 2, HEIGHT / 2));
}
