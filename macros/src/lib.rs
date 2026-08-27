use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DeriveInput, Expr, Fields, Type, parse_macro_input};

/// Derive macro that generates a `BusDebug` implementation for a struct.
///
/// Annotate fields with:
/// - `#[debug_device("Name")]` — field implements `Device`, listed in `devices()`.
///   Also generates `write_device_register()` and `reset_device()` dispatch. On
///   an array field `[T; N]` (literal `N`) it expands to N entries named
///   `"Name 1".."Name N"`, one per element.
/// - `#[debug_cpu("Name")]` — field implements `DebugCpu`, listed in both `devices()`
///   AND `cpus()`. Debug reads/writes are auto-routed through the matching
///   `#[debug_map(cpu = N)]` field's `debug_read`/`debug_write`.
/// - `#[debug_cpu("Name", read = "method", write = "method")]` — explicit version:
///   names `&self` / `&mut self` methods on the struct for side-effect-free memory access.
/// - `#[debug_map(cpu = N)]` — field is an `AddressSpace16` or `AddressSpace32`
///   linked to CPU index N. Generates watchpoint routing, `peek`
///   (backed/I/O/unmapped semantics via `debug_peek`), and (when linked to a
///   `#[debug_cpu]`) debug memory access. The address width is inferred from the
///   field type: `AddressSpace16` maps clamp debug addresses to 16 bits, while
///   `AddressSpace32` maps (24/32-bit M68000 buses) route them untruncated.
///   `memory_map()` exposes 16-bit maps only (its return type is `&AddressSpace16`).
/// - `#[debug_bus]` — field's type also implements `BusDebug`; its devices, CPUs,
///   maps and watchpoints are merged into this one. Used when a system struct
///   owns the CPU separately from the board that owns the address space and
///   devices, so that `cpu.execute_cycle(&mut board, ..)` borrow-checks at a
///   concrete type. Local entries come first, so a `#[debug_cpu]` here keeps
///   index 0 and the nested board's devices follow.
///
/// CPU index assignment is positional: first `#[debug_cpu]` is index 0, etc.
/// Device indices for `write_device_register` / `reset_device` match `devices()` order.
#[proc_macro_derive(BusDebug, attributes(debug_device, debug_cpu, debug_map, debug_bus))]
pub fn derive_bus_debug(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("BusDebug can only be derived on structs with named fields"),
        },
        _ => panic!("BusDebug can only be derived on structs"),
    };

    // (name, accessor-expr, is_device) for all annotated fields, in field
    // order. `accessor` is the `self.field` (or `self.field[i]` for array
    // device fields) token stream, so device indices match `devices()` order.
    let mut device_entries: Vec<(syn::LitStr, TokenStream2, bool)> = Vec::new();
    let mut cpu_entries: Vec<(
        syn::LitStr,
        syn::Ident,
        Option<syn::LitStr>,
        Option<syn::LitStr>,
    )> = Vec::new(); // (name, field_ident, read_method?, write_method?)
    let mut map_entries: Vec<MapEntry> = Vec::new(); // (cpu_index, field_ident, is_32) for AddressSpace fields
    let mut bus_field: Option<syn::Ident> = None; // #[debug_bus] — nested BusDebug to merge

    for field in fields {
        let field_ident = field.ident.as_ref().expect("named field");

        for attr in &field.attrs {
            if attr.path().is_ident("debug_bus") {
                assert!(
                    bus_field.is_none(),
                    "BusDebug allows only one #[debug_bus] field"
                );
                bus_field = Some(field_ident.clone());
                continue;
            }
            if attr.path().is_ident("debug_device") {
                // #[debug_device("Name")] — field implements Device. On an
                // array field `[T; N]`, expands to N entries "Name 1".."Name N".
                let name: syn::LitStr = attr
                    .parse_args()
                    .expect("debug_device expects a string literal: #[debug_device(\"Name\")]");
                if let Some(len) = array_len(&field.ty) {
                    for i in 0..len {
                        let elem_name =
                            syn::LitStr::new(&format!("{} {}", name.value(), i + 1), name.span());
                        device_entries.push((elem_name, quote! { self.#field_ident[#i] }, true));
                    }
                } else {
                    device_entries.push((name, quote! { self.#field_ident }, true));
                }
            } else if attr.path().is_ident("debug_cpu") {
                // #[debug_cpu("Name")] or #[debug_cpu("Name", read = "method", write = "method")]
                let args: CpuArgs = attr
                    .parse_args()
                    .expect("debug_cpu expects: (\"Name\") or (\"Name\", read = \"method\", write = \"method\")");
                // CPUs appear in both devices() and cpus()
                device_entries.push((args.name.clone(), quote! { self.#field_ident }, false));
                cpu_entries.push((args.name, field_ident.clone(), args.read, args.write));
            } else if attr.path().is_ident("debug_map") {
                // #[debug_map(cpu = N)] — field is an AddressSpace{16,32} linked
                // to CPU index N. The address width is inferred from the field
                // type so 24/32-bit (M68000) buses route addresses untruncated.
                let args: MapArgs = attr.parse_args().expect("debug_map expects: (cpu = N)");
                map_entries.push(MapEntry {
                    cpu_index: args.cpu_index,
                    field_ident: field_ident.clone(),
                    is_32: type_is(&field.ty, "AddressSpace32"),
                });
            }
        }
    }

    // Generate devices() body
    let device_items = device_entries.iter().map(|(name, accessor, _)| {
        quote! { (#name, &#accessor as &dyn phosphor_core::core::debug::Debuggable) }
    });

    // Generate cpus() body
    let cpu_items = cpu_entries.iter().map(|(name, ident, _, _)| {
        quote! { (#name, &self.#ident as &dyn phosphor_core::core::debug::DebugCpu) }
    });

    // A CPU whose memory access is neither given explicitly nor served by a
    // local map must be reachable through a #[debug_bus] field — otherwise the
    // debugger would silently read nothing for it.
    for (i, (_, _, read_method, _)) in cpu_entries.iter().enumerate() {
        assert!(
            read_method.is_some()
                || map_entries.iter().any(|m| m.cpu_index == i)
                || bus_field.is_some(),
            "debug_cpu at index {i} has no read/write methods, no matching #[debug_map(cpu = {i})], and no #[debug_bus] field"
        );
    }

    // Memory access arms are keyed by CPU index. A `#[debug_cpu]` with explicit
    // read/write methods supplies its own; otherwise the arm comes from the
    // `#[debug_map(cpu = N)]` at that index — which exists whether or not the
    // CPU itself lives in this struct, so a board split away from its CPU still
    // serves debug reads. Indices with neither fall through to `#[debug_bus]`.
    let explicit: Vec<(usize, &syn::LitStr, &syn::LitStr)> = cpu_entries
        .iter()
        .enumerate()
        .filter_map(|(i, (_, _, read, write))| Some((i, read.as_ref()?, write.as_ref()?)))
        .collect();
    let mapped: Vec<&MapEntry> = map_entries
        .iter()
        .filter(|m| !explicit.iter().any(|(i, _, _)| *i == m.cpu_index))
        .collect();

    // Generate read() match arms
    let read_arms: Vec<_> = explicit
        .iter()
        .map(|(idx, read_method, _)| {
            // Explicit method: self.method(addr) — 16-bit addressed.
            let read_ident = syn::Ident::new(read_method.value().as_str(), read_method.span());
            quote! { #idx => u16::try_from(addr).ok().and_then(|addr| self.#read_ident(addr)) }
        })
        .chain(mapped.iter().map(|map_field| {
            // 32-bit maps take the address untruncated; 16-bit maps clamp to
            // their space.
            let idx = map_field.cpu_index;
            let map_ident = &map_field.field_ident;
            if map_field.is_32 {
                quote! { #idx => self.#map_ident.debug_read(addr) }
            } else {
                quote! { #idx => u16::try_from(addr).ok().and_then(|addr| self.#map_ident.debug_read(addr)) }
            }
        }))
        .collect();

    // Generate write() match arms
    let write_arms: Vec<_> = explicit
        .iter()
        .map(|(idx, _, write_method)| {
            // Explicit method: self.method(addr, data) — 16-bit addressed.
            let write_ident = syn::Ident::new(write_method.value().as_str(), write_method.span());
            quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#write_ident(addr, data); } } }
        })
        .chain(mapped.iter().map(|map_field| {
            let idx = map_field.cpu_index;
            let map_ident = &map_field.field_ident;
            if map_field.is_32 {
                quote! { #idx => self.#map_ident.debug_write(addr, data) }
            } else {
                quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#map_ident.debug_write(addr, data); } } }
            }
        }))
        .collect();

    // Generate poke() match arms — a *debugger* write that records a Frontend
    // trace event. Mirrors write(), but the map path routes to `poke` (tagged)
    // instead of `debug_write` (untagged). No board uses explicit write methods
    // today, so that branch just falls back to the (untagged) method.
    let poke_arms: Vec<_> = explicit
        .iter()
        .map(|(idx, _, write_method)| {
            let write_ident = syn::Ident::new(write_method.value().as_str(), write_method.span());
            quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#write_ident(addr, data); } } }
        })
        .chain(mapped.iter().map(|map_field| {
            let idx = map_field.cpu_index;
            let map_ident = &map_field.field_ident;
            if map_field.is_32 {
                quote! { #idx => self.#map_ident.poke(addr, data) }
            } else {
                quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#map_ident.poke(addr, data); } } }
            }
        }))
        .collect();

    // Generate write_device_register() match arms (only #[debug_device] fields)
    let device_write_arms =
        device_entries
            .iter()
            .enumerate()
            .filter_map(|(i, (_, accessor, is_device))| {
                if *is_device {
                    let idx = i;
                    Some(quote! {
                        #idx => phosphor_core::device::Device::write(&mut #accessor, offset, data)
                    })
                } else {
                    None
                }
            });

    // Generate reset_device() match arms (only #[debug_device] fields)
    let device_reset_arms =
        device_entries
            .iter()
            .enumerate()
            .filter_map(|(i, (_, accessor, is_device))| {
                if *is_device {
                    let idx = i;
                    Some(quote! {
                        #idx => phosphor_core::device::Device::reset(&mut #accessor)
                    })
                } else {
                    None
                }
            });

    // Fall-through arms. With a `#[debug_bus]` field, anything this struct does
    // not answer for itself is forwarded to the nested bus; device indices are
    // rebased past the local entries so `devices()` order stays the index space.
    let local_device_count = device_entries.len();
    let (
        read_fallback,
        write_fallback,
        poke_fallback,
        device_write_fallback,
        device_reset_fallback,
    ) = match &bus_field {
        Some(bus) => (
            quote! { _ => phosphor_core::core::debug::BusDebug::read(&self.#bus, cpu_index, addr) },
            quote! { _ => phosphor_core::core::debug::BusDebug::write(&mut self.#bus, cpu_index, addr, data) },
            quote! { _ => phosphor_core::core::debug::BusDebug::poke(&mut self.#bus, cpu_index, addr, data) },
            quote! {
                i if i >= #local_device_count => phosphor_core::core::debug::BusDebug::write_device_register(
                    &mut self.#bus, i - #local_device_count, offset, data
                ),
                _ => {}
            },
            quote! {
                i if i >= #local_device_count => phosphor_core::core::debug::BusDebug::reset_device(
                    &mut self.#bus, i - #local_device_count
                ),
                _ => {}
            },
        ),
        None => (
            quote! { _ => None },
            quote! { _ => {} },
            quote! { _ => {} },
            quote! { _ => {} },
            quote! { _ => {} },
        ),
    };

    // devices()/cpus(): local entries first, then the nested bus's.
    let (devices_tail, cpus_tail) = match &bus_field {
        Some(bus) => (
            quote! { items.extend(phosphor_core::core::debug::BusDebug::devices(&self.#bus)); },
            quote! { items.extend(phosphor_core::core::debug::BusDebug::cpus(&self.#bus)); },
        ),
        None => (quote! {}, quote! {}),
    };

    // Generate watchpoint methods (needed when this struct owns maps, or when a
    // nested `#[debug_bus]` owns them on its behalf)
    let watchpoint_methods = if !map_entries.is_empty() || bus_field.is_some() {
        // Nested-bus fall-throughs for the address-space-shaped methods.
        let (
            peek_fallback,
            take_hit_tail,
            set_fallback,
            set_cond_fallback,
            clear_fallback,
            clear_all_tail,
            memory_map_fallback,
        ) = match &bus_field {
            Some(bus) => (
                quote! { _ => phosphor_core::core::debug::BusDebug::peek(&self.#bus, cpu_index, addr) },
                quote! { .or_else(|| phosphor_core::core::debug::BusDebug::take_watchpoint_hit(&mut self.#bus)) },
                quote! { _ => phosphor_core::core::debug::BusDebug::set_watchpoint(&mut self.#bus, cpu_index, addr, kind) },
                quote! { _ => phosphor_core::core::debug::BusDebug::set_watchpoint_cond(&mut self.#bus, cpu_index, addr, kind, condition) },
                quote! { _ => phosphor_core::core::debug::BusDebug::clear_watchpoint(&mut self.#bus, cpu_index, addr, kind) },
                quote! { phosphor_core::core::debug::BusDebug::clear_all_watchpoints(&mut self.#bus); },
                quote! { _ => phosphor_core::core::debug::BusDebug::memory_map(&self.#bus, cpu_index) },
            ),
            None => (
                // No nested bus: fall back to the read()-based default semantics.
                quote! {
                    _ => match self.read(cpu_index, addr) {
                        Some(value) => phosphor_core::core::DebugRead::Backed {
                            value: value as u32,
                            width: 1,
                            region_id: 0,
                        },
                        None => phosphor_core::core::DebugRead::Unmapped,
                    }
                },
                quote! {},
                quote! { _ => {} },
                quote! { _ => {} },
                quote! { _ => {} },
                quote! {},
                quote! { _ => None },
            ),
        };
        // take_watchpoint_hit: chain .or_else() across all maps (declaration order)
        let take_hit_chain = map_entries.iter().map(|entry| {
            let ident = &entry.field_ident;
            quote! { .or_else(|| self.#ident.take_hit()) }
        });

        // set_watchpoint / clear_watchpoint: match on cpu_index, passing it
        // through so hits record which CPU's address space fired. 16-bit maps
        // clamp the address (>0xFFFF can never fire); 32-bit maps take it whole.
        let set_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            if entry.is_32 {
                quote! { #idx => self.#ident.set_watchpoint(cpu_index, addr, kind) }
            } else {
                quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#ident.set_watchpoint(cpu_index, addr, kind); } } }
            }
        });
        // set_watchpoint_cond: mirrors set_watchpoint, threading the condition.
        let set_cond_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            if entry.is_32 {
                quote! { #idx => self.#ident.set_watchpoint_cond(cpu_index, addr, kind, condition) }
            } else {
                quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#ident.set_watchpoint_cond(cpu_index, addr, kind, condition); } } }
            }
        });
        let clear_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            if entry.is_32 {
                quote! { #idx => self.#ident.clear_watchpoint(cpu_index, addr, kind) }
            } else {
                quote! { #idx => { if let Ok(addr) = u16::try_from(addr) { self.#ident.clear_watchpoint(cpu_index, addr, kind); } } }
            }
        });

        // clear_all_watchpoints: call on every map
        let clear_all_calls = map_entries.iter().map(|entry| {
            let ident = &entry.field_ident;
            quote! { self.#ident.clear_all_watchpoints(); }
        });

        // memory_map: 16-bit maps only — the trait returns `&AddressSpace16`,
        // so 32-bit maps are omitted (the debugger reads them via `peek`).
        let map_arms: Vec<_> = map_entries
            .iter()
            .filter(|entry| !entry.is_32)
            .map(|entry| {
                let idx = entry.cpu_index;
                let ident = &entry.field_ident;
                quote! { #idx => Some(&self.#ident) }
            })
            .collect();
        let memory_map_body = quote! {
            match cpu_index {
                #(#map_arms,)*
                #memory_map_fallback,
            }
        };

        // peek: per-map backed/io/unmapped semantics; CPUs without a map
        // fall back to the read()-based default semantics. 16-bit maps report
        // addresses above their space as unmapped.
        let peek_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            if entry.is_32 {
                quote! { #idx => self.#ident.debug_peek(addr) }
            } else {
                quote! {
                    #idx => match u16::try_from(addr) {
                        Ok(addr) => self.#ident.debug_peek(addr),
                        Err(_) => phosphor_core::core::DebugRead::Unmapped,
                    }
                }
            }
        });

        quote! {
            fn peek(&self, cpu_index: usize, addr: u32) -> phosphor_core::core::DebugRead {
                match cpu_index {
                    #(#peek_arms,)*
                    #peek_fallback,
                }
            }

            fn take_watchpoint_hit(&mut self) -> Option<phosphor_core::core::WatchpointHit> {
                None #(#take_hit_chain)* #take_hit_tail
            }

            fn set_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: phosphor_core::core::WatchpointKind) {
                match cpu_index {
                    #(#set_arms,)*
                    #set_fallback,
                }
            }

            fn set_watchpoint_cond(&mut self, cpu_index: usize, addr: u32, kind: phosphor_core::core::WatchpointKind, condition: phosphor_core::core::WatchpointCondition) {
                match cpu_index {
                    #(#set_cond_arms,)*
                    #set_cond_fallback,
                }
            }

            fn clear_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: phosphor_core::core::WatchpointKind) {
                match cpu_index {
                    #(#clear_arms,)*
                    #clear_fallback,
                }
            }

            fn clear_all_watchpoints(&mut self) {
                #(#clear_all_calls)*
                #clear_all_tail
            }

            fn memory_map(&self, cpu_index: usize) -> Option<&phosphor_core::core::AddressSpace16> {
                #memory_map_body
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl phosphor_core::core::debug::BusDebug for #struct_name {
            fn devices(&self) -> Vec<(&str, &dyn phosphor_core::core::debug::Debuggable)> {
                #[allow(unused_mut)]
                let mut items: Vec<(&str, &dyn phosphor_core::core::debug::Debuggable)> =
                    vec![#(#device_items),*];
                #devices_tail
                items
            }

            fn cpus(&self) -> Vec<(&str, &dyn phosphor_core::core::debug::DebugCpu)> {
                #[allow(unused_mut)]
                let mut items: Vec<(&str, &dyn phosphor_core::core::debug::DebugCpu)> =
                    vec![#(#cpu_items),*];
                #cpus_tail
                items
            }

            fn read(&self, cpu_index: usize, addr: u32) -> Option<u8> {
                // Address width is handled per-CPU arm: 16-bit maps/methods
                // clamp to their space, 32-bit maps take the address whole.
                match cpu_index {
                    #(#read_arms,)*
                    #read_fallback,
                }
            }

            fn write(&mut self, cpu_index: usize, addr: u32, data: u8) {
                match cpu_index {
                    #(#write_arms,)*
                    #write_fallback,
                }
            }

            fn poke(&mut self, cpu_index: usize, addr: u32, data: u8) {
                match cpu_index {
                    #(#poke_arms,)*
                    #poke_fallback,
                }
            }

            fn write_device_register(&mut self, device_index: usize, offset: u16, data: u8) {
                match device_index {
                    #(#device_write_arms,)*
                    #device_write_fallback
                }
            }

            fn reset_device(&mut self, device_index: usize) {
                match device_index {
                    #(#device_reset_arms,)*
                    #device_reset_fallback
                }
            }

            #watchpoint_methods
        }
    };

    TokenStream::from(expanded)
}

/// Parsed arguments for `#[debug_cpu("Name")]` or `#[debug_cpu("Name", read = "method", write = "method")]`.
struct CpuArgs {
    name: syn::LitStr,
    read: Option<syn::LitStr>,
    write: Option<syn::LitStr>,
}

impl syn::parse::Parse for CpuArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let name: syn::LitStr = input.parse()?;

        // If no comma follows, this is the short form: #[debug_cpu("Name")]
        if input.is_empty() || !input.peek(syn::Token![,]) {
            return Ok(CpuArgs {
                name,
                read: None,
                write: None,
            });
        }

        input.parse::<syn::Token![,]>()?;

        let mut read = None;
        let mut write = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: syn::LitStr = input.parse()?;

            match key.to_string().as_str() {
                "read" => read = Some(value),
                "write" => write = Some(value),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown attribute `{other}`, expected `read` or `write`"),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }

        // If read or write is provided, both must be present
        if read.is_some() != write.is_some() {
            return Err(input.error("both `read` and `write` must be specified, or neither"));
        }

        Ok(CpuArgs { name, read, write })
    }
}

/// Collected info for a `#[debug_map(cpu = N)]` field.
struct MapEntry {
    cpu_index: usize,
    field_ident: syn::Ident,
    /// True for `AddressSpace32` maps (24/32-bit buses); false for
    /// `AddressSpace16`. Controls whether debug addresses are truncated.
    is_32: bool,
}

/// True if `ty`'s final path segment is the identifier `name`
/// (e.g. `phosphor_core::core::AddressSpace32` matches `"AddressSpace32"`).
fn type_is(ty: &Type, name: &str) -> bool {
    matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == name))
}

/// Length of a fixed-size array type `[T; N]` when `N` is an integer literal;
/// `None` for non-array types (or non-literal lengths, which the derive does
/// not support for `#[debug_device]` arrays).
fn array_len(ty: &Type) -> Option<usize> {
    if let Type::Array(arr) = ty
        && let Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(n),
            ..
        }) = &arr.len
    {
        return n.base10_parse::<usize>().ok();
    }
    None
}

/// Parsed arguments for `#[debug_map(cpu = N)]`.
struct MapArgs {
    cpu_index: usize,
}

impl syn::parse::Parse for MapArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let key: syn::Ident = input.parse()?;
        if key != "cpu" {
            return Err(syn::Error::new(
                key.span(),
                format!("unknown attribute `{key}`, expected `cpu`"),
            ));
        }
        input.parse::<syn::Token![=]>()?;
        let value: syn::LitInt = input.parse()?;
        Ok(MapArgs {
            cpu_index: value.base10_parse()?,
        })
    }
}

/// Derive macro that generates a `DebugTrace` implementation for a struct
/// that embeds a `DebugTraceBuffer`.
///
/// Annotate exactly one field with `#[debug_events]`:
///
/// ```ignore
/// #[derive(DebugTrace)]
/// pub struct WilliamsBoard {
///     #[debug_events]
///     debug_trace: DebugTraceBuffer,
///     // ...
/// }
/// ```
///
/// Generates `set_trace_enabled`, `trace_enabled`, `trace_events`, and
/// `clear_trace_events` delegating to the annotated field.
#[proc_macro_derive(DebugTrace, attributes(debug_events))]
pub fn derive_debug_trace(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("DebugTrace can only be derived on structs with named fields"),
        },
        _ => panic!("DebugTrace can only be derived on structs"),
    };

    let mut buffer_field: Option<syn::Ident> = None;
    for field in fields {
        if field
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("debug_events"))
        {
            assert!(
                buffer_field.is_none(),
                "DebugTrace allows only one #[debug_events] field"
            );
            buffer_field = Some(field.ident.clone().expect("named field"));
        }
    }
    let buffer = buffer_field
        .expect("DebugTrace requires one #[debug_events] field holding a DebugTraceBuffer");

    let expanded = quote! {
        impl phosphor_core::core::debug_trace::DebugTrace for #struct_name {
            fn set_trace_enabled(&mut self, enabled: bool) {
                self.#buffer.set_enabled(enabled);
            }

            fn trace_enabled(&self) -> bool {
                self.#buffer.enabled()
            }

            fn trace_events(&mut self) -> &[phosphor_core::core::debug_trace::DebugEvent] {
                self.#buffer.events()
            }

            fn clear_trace_events(&mut self) {
                self.#buffer.clear();
            }
        }
    };

    TokenStream::from(expanded)
}

/// Derive macro that generates boilerplate for memory region ID enums.
///
/// Given a `#[repr(u8)]` enum, generates:
/// - `impl From<EnumName> for u8` (casting via `as u8`)
/// - Associated `u8` constants in SCREAMING_SNAKE_CASE for each variant
///   (e.g., `Region::VideoRam` → `Region::VIDEO_RAM`)
///
/// The constants inherit the enum's visibility.
#[proc_macro_derive(MemoryRegion)]
pub fn derive_memory_region(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let enum_name = &input.ident;
    let vis = &input.vis;

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => panic!("MemoryRegion can only be derived on enums"),
    };

    // Generate associated constants (PascalCase → SCREAMING_SNAKE_CASE)
    let const_items = variants.iter().map(|v| {
        let variant_name = &v.ident;
        let const_name = syn::Ident::new(
            &pascal_to_screaming_snake(&variant_name.to_string()),
            variant_name.span(),
        );
        quote! {
            #[allow(dead_code)]
            #vis const #const_name: u8 = Self::#variant_name as u8;
        }
    });

    let expanded = quote! {
        impl #enum_name {
            #(#const_items)*
        }

        impl From<#enum_name> for u8 {
            fn from(r: #enum_name) -> u8 {
                r as u8
            }
        }
    };

    TokenStream::from(expanded)
}

/// Convert PascalCase to SCREAMING_SNAKE_CASE.
///
/// Inserts `_` before each uppercase letter that follows a lowercase letter.
fn pascal_to_screaming_snake(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 && s.as_bytes()[i - 1].is_ascii_lowercase() {
            result.push('_');
        }
        result.extend(c.to_uppercase());
    }
    result
}

// ---------------------------------------------------------------------------
// #[derive(Saveable)] — auto-generate Saveable trait implementations
// ---------------------------------------------------------------------------

/// Derive macro that generates `Saveable` trait implementations for structs.
///
/// Annotate the struct with `#[save_version(N)]` to emit a version tag.
/// Annotate fields with `#[save_skip]` to exclude them from serialization.
///
/// # Field attributes
///
/// - `#[save_skip]` — field is not saved or loaded; keeps its current value.
/// - `#[save_skip(default)]` — not saved; set to `Default::default()` on load.
/// - `#[save_skip(default = <expr>)]` — not saved; set to `<expr>` on load.
///
/// # Struct attributes
///
/// - `#[save_version(N)]` — emit and check a version byte.
/// - `#[save_tlv]` — frame every field under an explicit id (below).
/// - `#[save_retired(3, 7)]` — ids that once existed and must not be reused.
/// - `#[save_after_load(a, b)]` — call the inherent methods `self.a()` and
///   `self.b()`, in that order, once the load has finished and every
///   `#[save_skip(default…)]` has been applied.
///
/// `#[save_after_load]` is a **last resort**, not the normal way to restore a
/// derived value. Where the derived thing can simply be saved, save it: a
/// palette expanded from palette RAM is `[(u8, u8, u8); N]`, which this derive
/// encodes, and a page table implied by a bank register belongs to the address
/// space, which saves its own. Both were rebuild calls once, and both were bugs
/// waiting for someone to forget the call. What is left for the hook is the
/// state a save deliberately does not carry: a device re-reading a clock or a
/// potentiometer from configuration the file has no business pinning, or a
/// value that must be brought back into range before anything indexes with it.
///
/// # Supported field types
///
/// Primitives (`u8`, `u16`, `u32`, `u64`, `i16`, `i32`, `i64`, `f32`, `f64`,
/// `bool`), byte arrays (`[u8; N]`), byte vectors (`Vec<u8>`), fixed-size
/// arrays of primitives or `Saveable` types (`[T; N]`), and any other type
/// that implements `Saveable` (delegated via `save_state`/`load_state`).
///
/// An array element may itself be a tuple of primitives, as an expanded
/// palette's `[(u8, u8, u8); N]` is, or another array, as a per-chip
/// per-channel register file's `[[u8; 3]; 2]` is. Both are written flat: every
/// dimension's length and every tuple's arity is fixed by the type, so nothing
/// has to carry them.
///
/// # Chunk framing
///
/// Primitives and blobs are written inline. Every *nested component*, meaning a
/// field whose bytes come from another `Saveable` impl (an array of them
/// included), is wrapped by this parent in a `tag | len | payload` chunk, and
/// read back
/// through a reader bounded to that payload. A component whose body changes
/// therefore fails against its own name and cannot misread its siblings, and
/// only machines containing it lose their saves.
///
/// Tags are ordinals over the nested components in declaration order, starting
/// at 1 (tag 0 is reserved), so **field order is still wire order**. Inserting
/// or removing a component changes how many chunks the body holds and is always
/// caught. *Reordering* is not: it renumbers both components, so the tags still
/// line up and only the bodies disagree, so two components that encode alike
/// swap silently. Reordering components is a wire change; bump this struct's
/// `#[save_version]` when you do it.
///
/// # Field TLV (`#[save_tlv]`)
///
/// A struct marked `#[save_tlv]` frames **every** saved field, not just its
/// nested components, under an explicit id:
///
/// ```text
/// body      := version:u8 | count:u16 | field_tlv{count}
/// field_tlv := id:u16 | len:u32 | payload
/// ```
///
/// The reader dispatches on id rather than position, so declaration order stops
/// being wire order and an id it does not recognise is skipped by length. The
/// payload drops the redundant inner length that the positional encoding needs:
/// `[u8; N]` and `Vec<u8>` write raw bytes, because `len` already is the length.
///
/// `count` is what makes the body **self-delimiting**, and it is not optional.
/// A dispatch loop with no count runs to the end of whatever reader it is
/// handed, which is its own bytes only when a parent framed it; the 49
/// hand-written `Saveable` impls frame nothing, so a TLV struct inside one would
/// read straight through its parent's remaining fields. The count means a struct
/// can be opted in without auditing everyone who embeds it. (A child still
/// cannot know its own *tag*, which is why parents keep doing the framing.)
///
/// The writer emits fields in ascending id order rather than declaration order,
/// so the bytes are a function of the ids alone.
///
/// A TLV struct must carry `#[save_version(N)]`. The two mechanisms answer
/// different questions and are both needed: TLV absorbs *additive* change
/// (a field appears or disappears), the version byte catches *semantic* change
/// (`u16` widening to `u32`, or an existing field being reinterpreted).
///
/// Structs without `#[save_tlv]` keep positional bodies, and the two
/// interoperate in either direction: a TLV struct nested in a positional parent
/// is framed by the parent's ordinal tag, and a positional struct nested in a
/// TLV parent is framed by its `#[save(id = N)]`.
///
/// ## Field attributes under TLV
///
/// - `#[save(id = N)]` is required. Every saved field needs one; an id absent
///   from the file fails the load naming the field.
/// - `#[save(id = N, default)]` may be absent, in which case the field keeps
///   the value it was constructed with. This is how a newly added field stays
///   compatible with saves written before it existed. It is opt-in rather than
///   the default because a silently absent field leaves a device at power-on
///   while the rest of the machine is at frame N, which is the failure chunk
///   framing exists to make loud.
/// - `#[save_skip]` and its `(default)` / `(default = expr)` forms behave
///   exactly as they do positionally.
///
/// ## Optional components
///
/// An `Option<T>` field is written exactly when it is `Some`, and its id is
/// simply absent from the body otherwise. That is how a per-variant component
/// — a speech board fitted on one game and not its sibling — is expressed
/// without a presence flag of its own.
///
/// Both directions of the mismatch fail, naming the field: a file that carries
/// the component into a machine without one, and a machine with one fitted
/// loading a file that has none. Adding `default` waives the second, for a
/// component introduced after the saves were written. An `Option` field in a
/// positional struct is rejected, because a positional body writes every field
/// and so has nowhere to say a component is absent.
///
/// ## Retiring an id
///
/// `#[save_retired(3, 7)]` at the struct level lists ids that once existed and
/// must never be reused. The derive asserts no live field collides with one,
/// so "never reuse a tag" is checked rather than left to reviewer discipline.
/// Readers already skip an unknown id, so a retired one needs nothing at load
/// time; the attribute exists purely for the assertion.
///
/// Ids are assigned by hand. Hashing field names was rejected: a rename would
/// silently change the wire.
///
/// # Fieldless enums
///
/// Deriving on an enum whose variants all carry no data writes one byte: the
/// variant's Rust discriminant, so `#[repr(u8)]` with `= N` on each variant and
/// a bare `enum` counting from zero both encode the way the source reads. A
/// byte no variant claims fails the load naming the type, rather than landing
/// on the first variant the way a hand-written `match` with a `_` arm does.
///
/// `#[save_tlv]` is rejected on an enum: one byte has no fields to give ids to.
/// `#[save_version(N)]` still works, and means what it does on a struct.
#[proc_macro_derive(
    Saveable,
    attributes(save_version, save_skip, save_tlv, save, save_retired, save_after_load)
)]
pub fn derive_saveable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    // Parse #[save_version(N)] from struct attributes
    let version = parse_save_version(&input.attrs);
    let tlv = input.attrs.iter().any(|a| a.path().is_ident("save_tlv"));
    let retired = parse_save_retired(&input.attrs);
    let after_load = parse_save_after_load(&input.attrs);

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Saveable can only be derived on structs with named fields"),
        },
        // A fieldless enum is one byte, not a body of fields, so it takes the
        // whole path of its own.
        syn::Data::Enum(data) => {
            if !after_load.is_empty() {
                panic!("{struct_name}: #[save_after_load] has no meaning on an enum");
            }
            return gen_unit_enum(struct_name, data, version, tlv, &retired);
        }
        _ => panic!("Saveable can only be derived on structs and fieldless enums"),
    };

    if !tlv {
        if !retired.is_empty() {
            panic!("{struct_name}: #[save_retired] only means anything with #[save_tlv]");
        }
        if let Some(field) = fields.iter().find(|f| has_save_id_attr(&f.attrs)) {
            let ident = field.ident.as_ref().expect("named field");
            panic!(
                "{struct_name}.{ident}: #[save(id = ...)] needs #[save_tlv] on the struct, \
                 or the id would be silently ignored"
            );
        }
    } else if version.is_none() {
        panic!(
            "{struct_name}: #[save_tlv] needs #[save_version(N)]. TLV absorbs additive change; \
             the version byte is what catches a field changing meaning."
        );
    }

    let version_write = version.map(|v| quote! { w.write_version(#v); });
    let version_read = version.map(|v| quote! { r.read_version(#v)?; });

    let mut load_skip_stmts = Vec::new();

    for field in fields {
        let ident = field.ident.as_ref().expect("named field");
        match parse_save_skip(&field.attrs) {
            SaveSkip::None | SaveSkip::Keep => {}
            SaveSkip::Default => {
                // #[save_skip(default)] — set to Default::default() on load
                load_skip_stmts.push(quote! { self.#ident = Default::default(); });
            }
            SaveSkip::Expr(expr) => {
                // #[save_skip(default = <expr>)] — set to expr on load
                load_skip_stmts.push(quote! { self.#ident = #expr; });
            }
        }
    }

    let (save_body, load_body) = if tlv {
        gen_tlv_body(struct_name, fields, &retired)
    } else {
        gen_positional_body(struct_name, fields)
    };

    let expanded = quote! {
        impl phosphor_core::prelude::Saveable for #struct_name {
            fn save_state(&self, w: &mut phosphor_core::prelude::StateWriter) {
                #version_write
                #save_body
            }

            fn load_state(
                &mut self,
                r: &mut phosphor_core::prelude::StateReader,
            ) -> Result<(), phosphor_core::prelude::SaveError> {
                #version_read
                #load_body
                #(#load_skip_stmts)*
                #(self.#after_load();)*
                Ok(())
            }
        }
    };

    TokenStream::from(expanded)
}

/// A fieldless enum: one byte carrying the variant's discriminant.
///
/// The discriminants are Rust's own, so `#[repr(u8)]` with `= N` on each variant
/// and a bare `enum` counting from zero both encode the way the source reads.
/// The variant a byte names is looked up rather than transmuted, so a value no
/// variant claims **fails the load naming the type** rather than landing on the
/// first variant. Every hand-written impl this replaces fell back to variant
/// zero, which is a corrupt save resuming as a plausible one.
fn gen_unit_enum(
    name: &syn::Ident,
    data: &syn::DataEnum,
    version: Option<u8>,
    tlv: bool,
    retired: &[u16],
) -> TokenStream {
    if tlv {
        panic!(
            "{name}: #[save_tlv] has no meaning on an enum. A fieldless enum is a \
             single byte, so there are no fields to give ids to."
        );
    }
    if !retired.is_empty() {
        panic!("{name}: #[save_retired] only means anything with #[save_tlv]");
    }

    let mut next: u8 = 0;
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let mut assigned: Vec<(u8, String)> = Vec::new();

    for variant in &data.variants {
        let ident = &variant.ident;
        if !matches!(variant.fields, Fields::Unit) {
            panic!("{name}::{ident}: Saveable supports fieldless enums only");
        }
        let value = match &variant.discriminant {
            Some((_, expr)) => parse_u8_literal(expr).unwrap_or_else(|| {
                panic!(
                    "{name}::{ident}: a discriminant must be a literal 0-255 for the \
                     derive to encode it"
                )
            }),
            None => next,
        };
        if let Some((_, other)) = assigned.iter().find(|(v, _)| *v == value) {
            panic!("{name}::{ident}: discriminant {value} is already used by {other}");
        }
        assigned.push((value, format!("{name}::{ident}")));
        next = value
            .checked_add(1)
            .unwrap_or_else(|| panic!("{name}::{ident}: discriminants must fit in a u8"));

        writes.push(quote! { Self::#ident => #value, });
        reads.push(quote! { #value => Self::#ident, });
    }

    if writes.is_empty() {
        panic!("{name}: Saveable needs at least one variant to encode");
    }

    let version_write = version.map(|v| quote! { w.write_version(#v); });
    let version_read = version.map(|v| quote! { r.read_version(#v)?; });
    let type_name = name.to_string();

    let expanded = quote! {
        impl phosphor_core::prelude::Saveable for #name {
            fn save_state(&self, w: &mut phosphor_core::prelude::StateWriter) {
                #version_write
                w.write_u8(match self { #(#writes)* });
            }

            fn load_state(
                &mut self,
                r: &mut phosphor_core::prelude::StateReader,
            ) -> Result<(), phosphor_core::prelude::SaveError> {
                #version_read
                let __v = r.read_u8()?;
                *self = match __v {
                    #(#reads)*
                    other => {
                        return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                            format!("{} has no variant with discriminant {}", #type_name, other)
                        ));
                    }
                };
                Ok(())
            }
        }
    };

    TokenStream::from(expanded)
}

/// A discriminant expression that is a plain `u8` literal, which is the only
/// form the derive can turn into a wire byte.
fn parse_u8_literal(expr: &syn::Expr) -> Option<u8> {
    let syn::Expr::Lit(lit) = expr else {
        return None;
    };
    let syn::Lit::Int(int) = &lit.lit else {
        return None;
    };
    int.base10_parse::<u8>().ok()
}

/// Positional body: fields in declaration order, nested components framed under
/// an ordinal tag so a change to one cannot walk into its siblings.
fn gen_positional_body(
    struct_name: &syn::Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::Token![,]>,
) -> (TokenStream2, TokenStream2) {
    let mut save_stmts = Vec::new();
    let mut load_stmts = Vec::new();
    // Tag 0 is reserved, so the first component is 1.
    let mut next_tag: u16 = 1;

    for field in fields {
        if !matches!(parse_save_skip(&field.attrs), SaveSkip::None) {
            continue;
        }
        let ident = field.ident.as_ref().expect("named field");
        if option_inner_type(&field.ty).is_some() {
            panic!(
                "{struct_name}.{ident}: an Option field needs #[save_tlv]. A positional body \
                 has nowhere to say a component is absent, since every field is written."
            );
        }
        let (save, load) = gen_field_io(ident, &field.ty, Encoding::Positional);

        if delegates_to_saveable(&field.ty) {
            let tag = next_tag;
            next_tag = next_tag
                .checked_add(1)
                .filter(|t| *t != u16::MAX)
                .unwrap_or_else(|| {
                    panic!("{struct_name} has too many nested components for u16 tags")
                });
            let path = format!("{struct_name}.{ident}");
            save_stmts.push(quote! { w.write_tlv(#tag, |w| { #save }); });
            load_stmts.push(quote! { r.read_component(#tag, #path, |r| { #load Ok(()) })?; });
        } else {
            save_stmts.push(save);
            load_stmts.push(load);
        }
    }

    (quote! { #(#save_stmts)* }, quote! { #(#load_stmts)* })
}

/// TLV body: every saved field framed under its explicit id, read back by
/// dispatching on the id rather than on position.
fn gen_tlv_body(
    struct_name: &syn::Ident,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::Token![,]>,
    retired: &[u16],
) -> (TokenStream2, TokenStream2) {
    // Save statements are kept with their ids and emitted in ascending id
    // order, not declaration order. That makes the bytes a function of the ids
    // alone, so reordering fields in the source is a no-op on the wire in both
    // directions rather than only for the reader.
    let mut save_stmts: Vec<(u16, TokenStream2)> = Vec::new();
    let mut arms = Vec::new();
    let mut seen_decls = Vec::new();
    let mut missing_checks = Vec::new();
    let mut assigned: Vec<(u16, String)> = Vec::new();
    // Fields that are always written, and the `Option` ones that are written
    // only when fitted, which is why the count cannot be a constant.
    let mut fixed_count: u16 = 0;
    let mut optional_counts: Vec<TokenStream2> = Vec::new();
    let struct_path = struct_name.to_string();

    for field in fields {
        if !matches!(parse_save_skip(&field.attrs), SaveSkip::None) {
            continue;
        }
        let ident = field.ident.as_ref().expect("named field");
        let path = format!("{struct_name}.{ident}");

        let spec = parse_save_id(&field.attrs).unwrap_or_else(|| {
            panic!("{path}: #[save_tlv] structs need #[save(id = N)] on every saved field")
        });
        let id = spec.id;
        if id == 0 || id == u16::MAX {
            panic!("{path}: id {id} is reserved");
        }
        if let Some((_, other)) = assigned.iter().find(|(other_id, _)| *other_id == id) {
            panic!("{path}: id {id} is already used by {other}");
        }
        if retired.contains(&id) {
            panic!("{path}: id {id} is listed in #[save_retired] and must never be reused");
        }
        assigned.push((id, path.clone()));

        let seen = syn::Ident::new(&format!("__seen_{ident}"), ident.span());
        seen_decls.push(quote! { let mut #seen = false; });

        // An `Option<T>` component is present in the body exactly when it is
        // fitted on the machine, which is what makes a per-variant component
        // expressible without a presence flag of its own.
        if option_inner_type(&field.ty).is_some() {
            optional_counts.push(quote! {
                if self.#ident.is_some() {
                    __count += 1;
                }
            });
            save_stmts.push((
                id,
                quote! {
                    if let Some(__v) = &self.#ident {
                        w.write_tlv(#id, |w| {
                            phosphor_core::prelude::Saveable::save_state(__v, w);
                        });
                    }
                },
            ));
            arms.push(quote! {
                #id => {
                    if #seen {
                        return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                            format!("field id {} ({}) appears twice", #id, #path)
                        ));
                    }
                    #seen = true;
                    let Some(__v) = self.#ident.as_mut() else {
                        return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                            format!(
                                "{} is in the file but this machine has no such component",
                                #path
                            )
                        ));
                    };
                    r.read_payload(__id, __len, __at, #path, |r| {
                        phosphor_core::prelude::Saveable::load_state(__v, r)?;
                        Ok(())
                    })?;
                }
            });
            if !spec.default_if_absent {
                missing_checks.push(quote! {
                    if !#seen && self.#ident.is_some() {
                        return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                            format!(
                                "{} is fitted on this machine but absent from the file",
                                #path
                            )
                        ));
                    }
                });
            }
            continue;
        }

        fixed_count += 1;
        let (save, load) = gen_field_io(ident, &field.ty, Encoding::Tlv);
        save_stmts.push((id, quote! { w.write_tlv(#id, |w| { #save }); }));

        arms.push(quote! {
            #id => {
                if #seen {
                    return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                        format!("field id {} ({}) appears twice", #id, #path)
                    ));
                }
                #seen = true;
                r.read_payload(__id, __len, __at, #path, |r| { #load Ok(()) })?;
            }
        });
        if !spec.default_if_absent {
            missing_checks.push(quote! {
                if !#seen {
                    return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                        format!(
                            "required field {} (id {}) is absent; add `default` to its \
                             #[save(id = ...)] if it is meant to be optional",
                            #path, #id
                        )
                    ));
                }
            });
        }
    }

    let load = quote! {
        #(#seen_decls)*
        // The count is what makes a TLV body self-delimiting. Without it the
        // loop would run to the end of whatever reader it was handed, which is
        // only its own bytes when a parent framed it, and 49 hand-written
        // `Saveable` impls frame nothing.
        let __count = r.read_u16_le()?;
        for __i in 0..__count {
            // Captured before the header is read, so the trace and any error
            // point at the chunk header rather than past it.
            let __at = r.offset();
            let Some((__id, __len)) = r.read_tag_len()? else {
                return Err(phosphor_core::prelude::SaveError::InvalidFormat(
                    format!(
                        "{} declares {} fields but ran out after {}",
                        #struct_path, __count, __i
                    )
                ));
            };
            match __id {
                #(#arms)*
                // An id this build does not know: a field a newer build added,
                // or one retired here. Skipping by length is what makes either
                // harmless.
                _ => r.skip_unknown(__id, __len, __at)?,
            }
        }
        #(#missing_checks)*
    };

    save_stmts.sort_by_key(|(id, _)| *id);
    let save_stmts = save_stmts.into_iter().map(|(_, stmt)| stmt);
    let write_count = if optional_counts.is_empty() {
        quote! { w.write_u16_le(#fixed_count); }
    } else {
        quote! {
            let mut __count: u16 = #fixed_count;
            #(#optional_counts)*
            w.write_u16_le(__count);
        }
    };
    let save = quote! {
        #write_count
        #(#save_stmts)*
    };
    (save, load)
}

/// The `T` of an `Option<T>` field, which TLV writes only when it is `Some`.
fn option_inner_type(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let seg = path.path.segments.last().expect("non-empty path");
    if seg.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first() {
        Some(syn::GenericArgument::Type(inner)) => Some(inner),
        _ => None,
    }
}

/// `#[save(id = N)]` / `#[save(id = N, default)]` on a field.
struct SaveIdSpec {
    id: u16,
    /// The field may be absent from the file, keeping its constructed value.
    default_if_absent: bool,
}

impl syn::parse::Parse for SaveIdSpec {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let key: syn::Ident = input.parse()?;
        if key != "id" {
            return Err(syn::Error::new(
                key.span(),
                format!("unknown attribute `{key}`, expected `id`"),
            ));
        }
        input.parse::<syn::Token![=]>()?;
        let lit: syn::LitInt = input.parse()?;
        let id = lit.base10_parse::<u16>()?;

        let mut default_if_absent = false;
        if input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
            let flag: syn::Ident = input.parse()?;
            if flag != "default" {
                return Err(syn::Error::new(
                    flag.span(),
                    format!("unknown flag `{flag}`, expected `default`"),
                ));
            }
            default_if_absent = true;
        }
        Ok(SaveIdSpec {
            id,
            default_if_absent,
        })
    }
}

fn has_save_id_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("save"))
}

fn parse_save_id(attrs: &[syn::Attribute]) -> Option<SaveIdSpec> {
    attrs.iter().find(|a| a.path().is_ident("save")).map(|a| {
        a.parse_args()
            .expect("#[save] expects (id = N) or (id = N, default)")
    })
}

/// Extract `#[save_retired(3, 7)]` from struct-level attributes.
fn parse_save_retired(attrs: &[syn::Attribute]) -> Vec<u16> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("save_retired") {
            continue;
        }
        let ids = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::LitInt, syn::Token![,]>::parse_terminated,
            )
            .expect("#[save_retired] expects a comma-separated list of integers");
        for lit in ids {
            out.push(
                lit.base10_parse::<u16>()
                    .expect("#[save_retired] values must be u16"),
            );
        }
    }
    out
}

/// Extract `#[save_after_load(a, b)]` from struct-level attributes: inherent
/// methods to call, in order, once the load has finished.
fn parse_save_after_load(attrs: &[syn::Attribute]) -> Vec<syn::Ident> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("save_after_load") {
            continue;
        }
        let names = attr
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated,
            )
            .expect("#[save_after_load] expects a comma-separated list of method names");
        out.extend(names);
    }
    out
}

/// Extract `#[save_version(N)]` from struct-level attributes.
fn parse_save_version(attrs: &[syn::Attribute]) -> Option<u8> {
    for attr in attrs {
        if attr.path().is_ident("save_version") {
            let lit: syn::LitInt = attr
                .parse_args()
                .expect("#[save_version] expects an integer literal");
            return Some(
                lit.base10_parse::<u8>()
                    .expect("#[save_version] value must be u8"),
            );
        }
    }
    None
}

/// Parsed forms of `#[save_skip]`.
enum SaveSkip {
    /// No `#[save_skip]` attribute — normal serialized field.
    None,
    /// `#[save_skip]` — excluded, field keeps its current value on load.
    Keep,
    /// `#[save_skip(default)]` — excluded, set to `Default::default()` on load.
    Default,
    /// `#[save_skip(default = <expr>)]` — excluded, set to `<expr>` on load.
    Expr(Expr),
}

/// Parse `#[save_skip]`, `#[save_skip(default)]`, or `#[save_skip(default = <expr>)]`.
fn parse_save_skip(attrs: &[syn::Attribute]) -> SaveSkip {
    for attr in attrs {
        if attr.path().is_ident("save_skip") {
            // Check if the attribute has arguments
            match &attr.meta {
                syn::Meta::Path(_) => return SaveSkip::Keep,
                syn::Meta::List(list) => {
                    let args: SaveSkipArgs = syn::parse2(list.tokens.clone())
                        .expect("#[save_skip] expects empty, (default), or (default = <expr>)");
                    return match args.expr {
                        Some(expr) => SaveSkip::Expr(expr),
                        Option::None => SaveSkip::Default,
                    };
                }
                syn::Meta::NameValue(_) => {
                    panic!(
                        "#[save_skip] does not support = syntax; use #[save_skip(default = <expr>)]"
                    )
                }
            }
        }
    }
    SaveSkip::None
}

/// Parsed arguments for `#[save_skip(default)]` or `#[save_skip(default = <expr>)]`.
struct SaveSkipArgs {
    expr: Option<Expr>,
}

impl syn::parse::Parse for SaveSkipArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let key: syn::Ident = input.parse()?;
        if key != "default" {
            return Err(syn::Error::new(
                key.span(),
                format!("unknown attribute `{key}`, expected `default`"),
            ));
        }
        if input.peek(syn::Token![=]) {
            input.parse::<syn::Token![=]>()?;
            let expr: Expr = input.parse()?;
            Ok(SaveSkipArgs { expr: Some(expr) })
        } else {
            Ok(SaveSkipArgs { expr: Option::None })
        }
    }
}

/// Array element types written inline rather than framed as a component: a
/// primitive, a tuple of them such as an expanded palette's `(u8, u8, u8)`, or
/// an array of either, which is how a per-chip per-channel register file is
/// held.
fn is_inline_element_type(ty: &Type) -> bool {
    match ty {
        Type::Array(arr) => is_inline_element_type(&arr.elem),
        Type::Tuple(t) => !t.elems.is_empty() && t.elems.iter().all(is_primitive_type),
        _ => is_primitive_type(ty),
    }
}

/// The primitive type names `gen_field_io` encodes inline.
fn is_primitive_type(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let seg = path.path.segments.last().expect("non-empty path");
    matches!(
        seg.ident.to_string().as_str(),
        "u8" | "u16" | "u32" | "u64" | "i16" | "i32" | "i64" | "f32" | "f64" | "bool"
    )
}

/// Whether a field is a *nested component*, one whose bytes come from another
/// `Saveable` impl, rather than a primitive or a length-prefixed blob.
///
/// Components are the fields the parent frames in a chunk. Mirrors the branches
/// in [`gen_field_io`]: primitives, `[u8; N]`, arrays of primitives and
/// `Vec<u8>` are inline; everything else delegates.
fn delegates_to_saveable(ty: &Type) -> bool {
    match ty {
        // An array delegates only when its elements do; `[u8; N]`, `[u16; N]`
        // and `[(u8, u8, u8); N]` are inline.
        Type::Array(arr) => !is_inline_element_type(&arr.elem),
        Type::Path(path) => {
            if is_primitive_type(ty) {
                return false;
            }
            let seg = path.path.segments.last().expect("non-empty path");
            // A Box is transparent, so what it holds decides.
            if seg.ident == "Box" {
                return match generic_inner_type(seg) {
                    Some(inner) => delegates_to_saveable(inner),
                    None => true,
                };
            }
            // Vec<u8> is a length-prefixed blob. A Vec of anything else is
            // rejected by `gen_field_io`, so it never reaches a chunk.
            seg.ident != "Vec"
        }
        _ => true,
    }
}

/// How a field's payload is encoded, which differs only for bulk bytes.
#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    /// Positional body: a byte blob carries its own `u32` length, because
    /// nothing else in the stream says where it ends.
    Positional,
    /// TLV body: a byte blob is written raw, because the field's own `len`
    /// already is the length and repeating it would be redundant.
    Tlv,
}

/// Generate save and load token streams for one field of `self`.
fn gen_field_io(ident: &syn::Ident, ty: &Type, enc: Encoding) -> (TokenStream2, TokenStream2) {
    gen_place_io(&quote! { self.#ident }, ident, ty, enc)
}

/// Generate save and load token streams for the value at `place`, whose type is
/// `ty`. `ident` names the field for panic messages and loop bindings.
fn gen_place_io(
    place: &TokenStream2,
    ident: &syn::Ident,
    ty: &Type,
    enc: Encoding,
) -> (TokenStream2, TokenStream2) {
    match ty {
        // Fixed-size array: [T; N]
        Type::Array(arr) => gen_array_io(place, ident, &arr.elem, enc),
        // Path types: primitives, Vec<u8>, or Saveable delegates
        Type::Path(path) => {
            let seg = path.path.segments.last().expect("non-empty path");
            let type_name = seg.ident.to_string();
            match type_name.as_str() {
                "u8" => (
                    quote! { w.write_u8(#place); },
                    quote! { #place = r.read_u8()?; },
                ),
                "u16" => (
                    quote! { w.write_u16_le(#place); },
                    quote! { #place = r.read_u16_le()?; },
                ),
                "u32" => (
                    quote! { w.write_u32_le(#place); },
                    quote! { #place = r.read_u32_le()?; },
                ),
                "u64" => (
                    quote! { w.write_u64_le(#place); },
                    quote! { #place = r.read_u64_le()?; },
                ),
                "i16" => (
                    quote! { w.write_i16_le(#place); },
                    quote! { #place = r.read_i16_le()?; },
                ),
                "i32" => (
                    quote! { w.write_i32_le(#place); },
                    quote! { #place = r.read_i32_le()?; },
                ),
                "i64" => (
                    quote! { w.write_i64_le(#place); },
                    quote! { #place = r.read_i64_le()?; },
                ),
                "f32" => (
                    quote! { w.write_f32_le(#place); },
                    quote! { #place = r.read_f32_le()?; },
                ),
                "f64" => (
                    quote! { w.write_f64_le(#place); },
                    quote! { #place = r.read_f64_le()?; },
                ),
                "bool" => (
                    quote! { w.write_bool(#place); },
                    quote! { #place = r.read_bool()?; },
                ),
                // A `Box` is transparent on the wire: a 4 KB RAM held behind one
                // encodes exactly as it would inline. Dereferencing it here is
                // what lets the inner type's own encoding be reused verbatim.
                "Box" => {
                    let inner = generic_inner_type(seg).unwrap_or_else(|| {
                        panic!("Saveable derive: field `{ident}` has a Box with no type argument")
                    });
                    gen_place_io(&quote! { (*#place) }, ident, inner, enc)
                }
                "Vec" => {
                    // Verify it's Vec<u8>
                    if !is_vec_u8(seg) {
                        panic!(
                            "Saveable derive only supports Vec<u8>; field `{}` has unsupported Vec type",
                            ident
                        );
                    }
                    match enc {
                        Encoding::Positional => (
                            quote! { w.write_bytes(&#place); },
                            quote! { #place = r.read_bytes()?.to_vec(); },
                        ),
                        Encoding::Tlv => (
                            quote! { w.write_raw(&#place); },
                            quote! { #place = r.read_rest().to_vec(); },
                        ),
                    }
                }
                // Unknown type — delegate to Saveable
                _ => (
                    quote! { phosphor_core::prelude::Saveable::save_state(&#place, w); },
                    quote! { phosphor_core::prelude::Saveable::load_state(&mut #place, r)?; },
                ),
            }
        }
        _ => {
            // Fallback: delegate to Saveable
            (
                quote! { phosphor_core::prelude::Saveable::save_state(&#place, w); },
                quote! { phosphor_core::prelude::Saveable::load_state(&mut #place, r)?; },
            )
        }
    }
}

/// Generate save/load for `[T; N]` arrays.
fn gen_array_io(
    place: &TokenStream2,
    ident: &syn::Ident,
    elem_ty: &Type,
    enc: Encoding,
) -> (TokenStream2, TokenStream2) {
    // Bytes go in bulk rather than one at a time.
    if is_type_u8(elem_ty) {
        return match enc {
            Encoding::Positional => (
                quote! { w.write_bytes(&#place); },
                quote! { r.read_bytes_into(&mut #place)?; },
            ),
            Encoding::Tlv => (
                quote! { w.write_raw(&#place); },
                quote! { r.read_raw_into(&mut #place)?; },
            ),
        };
    }

    // For other element types, generate a loop
    let var = syn::Ident::new("__v0", ident.span());
    let (elem_save, elem_load) = gen_array_element_io(ident, &var, elem_ty, 0);
    (
        quote! { for #var in &#place { #elem_save } },
        quote! { for #var in &mut #place { #elem_load } },
    )
}

/// Generate per-element save/load for array loops.
/// Read/write statements for one primitive at `place`, or `None` if the type is
/// not a primitive.
///
/// `place` is the expression naming the value: `*__v` for an array element,
/// `__v.0` for a tuple field within one.
fn gen_primitive_io(place: &TokenStream2, ty: &Type) -> Option<(TokenStream2, TokenStream2)> {
    let Type::Path(path) = ty else { return None };
    let name = path
        .path
        .segments
        .last()
        .expect("non-empty path")
        .ident
        .to_string();
    let (write, read) = match name.as_str() {
        "u8" => (quote! { write_u8 }, quote! { read_u8 }),
        "u16" => (quote! { write_u16_le }, quote! { read_u16_le }),
        "u32" => (quote! { write_u32_le }, quote! { read_u32_le }),
        "u64" => (quote! { write_u64_le }, quote! { read_u64_le }),
        "i16" => (quote! { write_i16_le }, quote! { read_i16_le }),
        "i32" => (quote! { write_i32_le }, quote! { read_i32_le }),
        "i64" => (quote! { write_i64_le }, quote! { read_i64_le }),
        "f32" => (quote! { write_f32_le }, quote! { read_f32_le }),
        "f64" => (quote! { write_f64_le }, quote! { read_f64_le }),
        "bool" => (quote! { write_bool }, quote! { read_bool }),
        _ => return None,
    };
    Some((
        quote! { w.#write(#place); },
        quote! { #place = r.#read()?; },
    ))
}

/// Generate per-element save/load for array loops, for the element bound to
/// `var` at nesting `depth`.
///
/// `depth` names the loop variables of a nested array apart, so `[[u8; 3]; 2]`
/// binds `__v0` in the outer loop and `__v1` in the inner one.
fn gen_array_element_io(
    ident: &syn::Ident,
    var: &syn::Ident,
    elem_ty: &Type,
    depth: usize,
) -> (TokenStream2, TokenStream2) {
    // An array of arrays, such as the per-chip per-channel duty cycles a PSG
    // board keeps. One loop per dimension and no framing anywhere: every
    // dimension's length is fixed by the type, exactly as for a tuple.
    if let Type::Array(inner) = elem_ty {
        // A `[u8; M]` element goes in bulk, the same as a `[u8; N]` field. The
        // read takes exactly the element's length, so this stays correct inside
        // a loop where reading "the rest" would not be. Without it a
        // double-buffered bitmap would go a byte at a time.
        if is_type_u8(&inner.elem) {
            return (
                quote! { w.write_raw(#var); },
                quote! { r.read_raw_into(#var)?; },
            );
        }
        let next = syn::Ident::new(&format!("__v{}", depth + 1), var.span());
        let (save, load) = gen_array_element_io(ident, &next, &inner.elem, depth + 1);
        return (
            quote! { for #next in #var.iter() { #save } },
            quote! { for #next in #var.iter_mut() { #load } },
        );
    }

    // A tuple of primitives, such as the `[(u8, u8, u8); N]` an expanded palette
    // is held in. Written field by field, in order, with no framing: the array's
    // length and the tuple's arity are both fixed by the type.
    if let Type::Tuple(tuple) = elem_ty {
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        for (i, ty) in tuple.elems.iter().enumerate() {
            let index = syn::Index::from(i);
            let place = quote! { #var.#index };
            let (write, read) = gen_primitive_io(&place, ty).unwrap_or_else(|| {
                panic!(
                    "Saveable derive supports tuples of primitives only; field `{ident}` \
                     has a tuple element that is not one"
                )
            });
            writes.push(write);
            reads.push(read);
        }
        return (quote! { #(#writes)* }, quote! { #(#reads)* });
    }

    if let Some(io) = gen_primitive_io(&quote! { *#var }, elem_ty) {
        return io;
    }

    // Anything else is a nested component, framed by its parent.
    let _ = ident;
    (
        quote! { phosphor_core::prelude::Saveable::save_state(#var, w); },
        quote! { phosphor_core::prelude::Saveable::load_state(#var, r)?; },
    )
}

/// Check if a type is `u8`.
fn is_type_u8(ty: &Type) -> bool {
    if let Type::Path(path) = ty
        && let Some(seg) = path.path.segments.last()
    {
        return seg.ident == "u8";
    }
    false
}

/// Check if a path segment is `Vec<u8>`.
fn is_vec_u8(seg: &syn::PathSegment) -> bool {
    match generic_inner_type(seg) {
        Some(inner) => is_type_u8(inner),
        None => false,
    }
}

/// The first type argument of a segment like `Box<T>` or `Vec<T>`.
fn generic_inner_type(seg: &syn::PathSegment) -> Option<&Type> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first() {
        Some(syn::GenericArgument::Type(inner)) => Some(inner),
        _ => None,
    }
}
