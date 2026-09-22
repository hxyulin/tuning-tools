//! Standalone host for testing/embedding the isolated Damiao SDK worker.
fn main() {
    studio_carriers::can_process::run_worker_stdio();
}
