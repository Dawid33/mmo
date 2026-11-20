use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) struct MonotonicInstant {
    #[cfg(not(target_arch = "wasm32"))]
    at: std::time::Instant,
    #[cfg(target_arch = "wasm32")]
    at_ms: f64,
}

impl MonotonicInstant {
    pub(crate) fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self {
                at: std::time::Instant::now(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self {
                at_ms: web_now_ms(),
            }
        }
    }

    pub(crate) fn elapsed(self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.at.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Duration::from_secs_f64(((web_now_ms() - self.at_ms).max(0.0)) / 1000.0)
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn web_now_ms() -> f64 {
    use wasm_bindgen::JsCast as _;

    thread_local! {
        static PERFORMANCE: web_sys::Performance =
            js_sys::Reflect::get(&js_sys::global(), &"performance".into())
                .expect("read the browser performance clock")
                .dyn_into::<web_sys::Performance>()
                .expect("the browser performance clock has the expected type");
    }

    PERFORMANCE.with(web_sys::Performance::now)
}
