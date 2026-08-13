//! Realtime-contract audit for the block engine: after warm-up,
//! `SourceEngine::process` and `BusEngine::process` must perform zero heap
//! allocations, so the audio callback can never glitch on the allocator.
//!
//! This lives in its own integration binary because it installs a counting
//! global allocator. The counter is armed per-thread with a const-initialized
//! thread-local (no lazy TLS allocation inside `alloc`).

#![cfg(target_os = "linux")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use orion::domain::{EndpointId, RouteId};
use orion::realtime::{
    BusEngine, BusPlan, ChannelControls, ControlHub, PlanSlot, RouteLink, RouteMeter, SourceEngine,
    SourcePlan, TARGET_QUANTA,
};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(|armed| armed.get()) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn armed<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ARMED.with(|armed| armed.set(true));
    let before = ALLOCS.load(Ordering::Relaxed);
    let result = f();
    let count = ALLOCS.load(Ordering::Relaxed) - before;
    ARMED.with(|armed| armed.set(false));
    (result, count)
}

#[test]
fn process_is_allocation_free_after_warmup() {
    const CHANNELS: usize = 2;
    const FRAMES: usize = 128;
    let hub = ControlHub::default();

    let source_slot = Arc::new(PlanSlot::new(SourcePlan {
        generation: 0,
        feeds: Vec::new(),
    }));
    let bus_slot = Arc::new(PlanSlot::new(BusPlan {
        generation: 0,
        routes: Vec::new(),
    }));
    let source_controls = hub.endpoint(EndpointId::new());
    let (mut source, mut source_handle) = SourceEngine::new(
        CHANNELS,
        48_000,
        source_controls.clone(),
        Arc::new(ChannelControls::default()),
        Arc::new(RouteMeter::new(CHANNELS)),
        source_slot,
    )
    .expect("source engine");
    let (mut bus, mut bus_handle) = BusEngine::new(
        CHANNELS,
        48_000,
        hub.endpoint(EndpointId::new()),
        Arc::new(ChannelControls::default()),
        Arc::new(RouteMeter::new(CHANNELS)),
        bus_slot,
    )
    .expect("bus engine");

    // Link one route the way the backend does: plans first, then halves.
    let route_id = RouteId::new();
    let old_source = source_handle.plan_slot.publish(SourcePlan {
        generation: 1,
        feeds: vec![orion::realtime::RouteFeed {
            route_id,
            bus_channels: CHANNELS,
        }],
    });
    source_handle.reclaimer.retire(old_source);
    let old_bus = bus_handle.plan_slot.publish(BusPlan {
        generation: 1,
        routes: vec![route_id],
    });
    bus_handle.reclaimer.retire(old_bus);
    let link =
        RouteLink::new(route_id, CHANNELS, FRAMES as u32, TARGET_QUANTA, 48_000).expect("link");
    let (source_half, bus_half) = link
        .into_halves(1, 1, source_controls.balance_gains())
        .expect("halves");
    source_handle.inbox.push(source_half).expect("capacity");
    bus_handle.inbox.push(bus_half).expect("capacity");

    let input = vec![0.25_f32; FRAMES * CHANNELS];
    let mut output = vec![0.0_f32; FRAMES * CHANNELS];

    // Warm-up: activates the route (inbox/pending paths), settles smoothers.
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }
    // Backend-thread housekeeping is allowed to allocate; not audited here.
    while source_handle.retired.pop().is_ok() {}
    while bus_handle.retired.pop().is_ok() {}

    let ((), allocations) = armed(|| {
        for _ in 0..256 {
            source.process(&input);
            bus.process(&mut output);
        }
    });
    assert_eq!(
        allocations, 0,
        "engine process must not allocate after warm-up ({allocations} allocations)"
    );
}
