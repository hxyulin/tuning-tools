//! SWD inspection lab for STM32H723: reset HSI clock (64 MHz), DTCM data,
//! SysTick only. No GPIO, CAN, USB, motors or board peripherals are configured.
#![no_std]
#![no_main]

use core::{
    future::poll_fn,
    num::NonZeroU32,
    panic::PanicInfo,
    ptr,
    sync::atomic::{AtomicU32, Ordering::Relaxed},
    task::Poll,
};
use cortex_m::peripheral::syst::SystClkSource;
use cortex_m_rt::exception;
use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal, waitqueue::AtomicWaker,
};

#[path = "../rm_task_stats.rs"]
mod rm_task_stats;

static MILLIS: AtomicU32 = AtomicU32::new(0);
static CLOCK_WAKER: AtomicWaker = AtomicWaker::new();
static WORK: Signal<CriticalSectionRawMutex, u32> = Signal::new();
pub static UPDATES: AtomicU32 = AtomicU32::new(0);
pub static COMPLETED: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Mode {
    Idle = 0,
    Running = 1,
    Fault = 7,
}
#[derive(Clone, Copy)]
pub enum Command {
    Stop,
    Velocity(f32),
    Position { target: f32, limit: f32 },
}
#[derive(Clone, Copy)]
#[repr(C)]
pub union Bits {
    pub word: u32,
    pub float: f32,
    pub bytes: [u8; 4],
}
#[derive(Clone, Copy)]
pub struct Axis {
    pub position: f32,
    pub velocity: f32,
    pub gains: (f32, f32, f32),
}
#[derive(Clone, Copy)]
pub struct Scalars {
    pub unsigned: (u8, u16, u32, u64),
    pub signed: (i8, i16, i32, i64),
    pub enabled: bool,
    pub letter: char,
    pub finite: (f32, f64),
    pub special: [f32; 3],
}
#[derive(Clone, Copy)]
pub struct Robot {
    pub axes: [Axis; 2],
    pub matrix: [[i16; 4]; 3],
    pub mode: Mode,
    pub command: Command,
    pub optional: Option<f32>,
    pub niche: Option<NonZeroU32>,
    pub result: Result<u32, i16>,
    pub bits: Bits,
    pub scalars: Scalars,
}
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Link {
    pub value: u32,
    pub next: *const Link,
}

pub static mut ROBOT: Robot = Robot {
    axes: [Axis {
        position: 1.25,
        velocity: -2.5,
        gains: (1.0, 0.1, 0.01),
    }; 2],
    matrix: [[-1, 2, -3, 4], [5, -6, 7, -8], [9, 10, 11, 12]],
    mode: Mode::Idle,
    command: Command::Stop,
    optional: Some(3.5),
    niche: NonZeroU32::new(42),
    result: Ok(123),
    bits: Bits { word: 0x3f800000 },
    scalars: Scalars {
        unsigned: (255, 65535, u32::MAX, u64::MAX),
        signed: (-128, -32768, i32::MIN, i64::MIN),
        enabled: true,
        letter: 'λ',
        finite: (1.25, -2.5),
        special: [f32::NAN, f32::INFINITY, -0.0],
    },
};
pub static mut LARGE_ARRAY: [u16; 300] = [0; 300];
pub static mut LINK_A: Link = Link {
    value: 111,
    next: ptr::addr_of!(LINK_B),
};
pub static mut LINK_B: Link = Link {
    value: 222,
    next: ptr::addr_of!(LINK_A),
};
pub static mut ACTIVE_LINK: *const Link = ptr::addr_of!(LINK_A);
pub static mut LINK_SLOT: *const *const Link = ptr::addr_of!(ACTIVE_LINK);
pub static mut ARRAY_PTR: *const [u16; 300] = ptr::addr_of!(LARGE_ARRAY);
pub static mut TEXT: &str = "Robot λ — ready";
pub static mut EMPTY_TEXT: &str = "";
pub static mut SLICE: &[u16] = &[10, 20, 30, 40, 50];
pub static mut NULL_PTR: *const Robot = ptr::null();

#[exception]
fn SysTick() {
    MILLIS.fetch_add(10, Relaxed);
    studio_task_trace::tick(cortex_m::peripheral::DWT::cycle_count);
    CLOCK_WAKER.wake();
}

// Only the producer uses this clock future. Other tasks wait on signals/pending.
async fn delay(ms: u32) {
    let start = MILLIS.load(Relaxed);
    poll_fn(|cx| {
        CLOCK_WAKER.register(cx.waker());
        if MILLIS.load(Relaxed).wrapping_sub(start) >= ms {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

#[embassy_executor::task]
async fn producer() {
    let mut sequence = 0u32;
    loop {
        delay(750).await;
        sequence = sequence.wrapping_add(1);
        // This task is the sole writer; inspection is external SWD, not Rust references.
        unsafe {
            let mut robot = ptr::addr_of!(ROBOT).read_volatile();
            robot.axes[0].position = sequence as f32;
            robot.mode = match sequence % 3 {
                0 => Mode::Idle,
                1 => Mode::Running,
                _ => Mode::Fault,
            };
            robot.command = match sequence % 3 {
                0 => Command::Stop,
                1 => Command::Velocity(12.5),
                _ => Command::Position {
                    target: 90.0,
                    limit: 10.0,
                },
            };
            robot.optional = if sequence % 2 == 0 { None } else { Some(3.5) };
            robot.niche = NonZeroU32::new(if sequence % 2 == 0 { 0 } else { 42 });
            robot.result = if sequence % 2 == 0 {
                Ok(sequence)
            } else {
                Err(-7)
            };
            ptr::addr_of_mut!(ROBOT).write_volatile(robot);
            ptr::addr_of_mut!(ACTIVE_LINK).write_volatile(match sequence % 3 {
                0 => ptr::null(),
                1 => ptr::addr_of!(LINK_A),
                _ => ptr::addr_of!(LINK_B),
            });
            for i in 0..300 {
                ptr::addr_of_mut!(LARGE_ARRAY)
                    .cast::<u16>()
                    .add(i)
                    .write_volatile(i as u16);
            }
        }
        UPDATES.store(sequence, Relaxed);
        WORK.signal(sequence);
        delay(250).await;
    }
}

#[embassy_executor::task]
async fn consumer() {
    let mut local = Axis {
        position: 0.0,
        velocity: 2.0,
        gains: (3.0, 4.0, 5.0),
    };
    loop {
        let sequence = WORK.wait().await;
        local.position = sequence as f32;
        // A bounded burst gives the CPU/poll metrics something to distinguish.
        for i in 0..100_000 {
            core::hint::black_box(i);
        }
        core::hint::black_box(&local);
        COMPLETED.store(sequence, Relaxed);
    }
}

#[embassy_executor::task(pool_size = 2)]
async fn parked(id: u8) {
    let retained = (
        id,
        Axis {
            position: id as f32,
            velocity: 8.0,
            gains: (1.0, 2.0, 3.0),
        },
    );
    core::future::pending::<()>().await;
    core::hint::black_box(retained);
}

#[embassy_executor::task]
async fn finishes() {
    core::hint::black_box(42u32);
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    core::hint::black_box((
        ptr::addr_of!(ROBOT),
        ptr::addr_of!(LARGE_ARRAY),
        ptr::addr_of!(LINK_A),
        ptr::addr_of!(LINK_B),
        ptr::addr_of!(ACTIVE_LINK),
        ptr::addr_of!(LINK_SLOT),
        ptr::addr_of!(ARRAY_PTR),
        ptr::addr_of!(NULL_PTR),
        ptr::addr_of!(TEXT),
        ptr::addr_of!(EMPTY_TEXT),
        ptr::addr_of!(SLICE),
        ptr::addr_of!(UPDATES),
        ptr::addr_of!(COMPLETED),
    ));
    rm_task_stats::init(64_000_000);
    studio_task_trace::init(64_000_000, cortex_m::peripheral::DWT::cycle_count());
    let mut core = cortex_m::Peripherals::take().unwrap();
    core.SYST.set_clock_source(SystClkSource::Core);
    core.SYST.set_reload(640_000 - 1);
    core.SYST.clear_current();
    core.SYST.enable_interrupt();
    core.SYST.enable_counter();
    spawner.spawn(producer().unwrap());
    spawner.spawn(consumer().unwrap());
    spawner.spawn(parked(0).unwrap()); // Slot two is deliberately never spawned.
    spawner.spawn(finishes().unwrap());
    core::future::pending::<()>().await;
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {
        cortex_m::asm::bkpt();
    }
}
