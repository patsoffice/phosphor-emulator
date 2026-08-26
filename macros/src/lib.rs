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
/// - `#[save_elements]` — serialize `[u8; N]` per-element instead of bulk
///   `write_bytes`/`read_bytes_into`. Use when compatibility with existing
///   save formats that use individual `write_u8` calls is required.
///
/// # Supported field types
///
/// Primitives (`u8`, `u16`, `u32`, `u64`, `i16`, `i32`, `i64`, `f32`, `f64`,
/// `bool`), byte arrays (`[u8; N]`), byte vectors (`Vec<u8>`), fixed-size
/// arrays of primitives or `Saveable` types (`[T; N]`), and any other type
/// that implements `Saveable` (delegated via `save_state`/`load_state`).
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
/// `#[save_version]` when you do it. Explicit stable ids that survive a reorder
/// are Stage B (`phosphor-emulator-tlv-save-state-hc61.3`).
#[proc_macro_derive(Saveable, attributes(save_version, save_skip, save_elements))]
pub fn derive_saveable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;

    let fields = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => panic!("Saveable can only be derived on structs with named fields"),
        },
        _ => panic!("Saveable can only be derived on structs"),
    };

    // Parse #[save_version(N)] from struct attributes
    let version = parse_save_version(&input.attrs);

    let version_write = version.map(|v| quote! { w.write_version(#v); });
    let version_read = version.map(|v| quote! { r.read_version(#v)?; });

    let mut save_stmts = Vec::new();
    let mut load_stmts = Vec::new();
    let mut load_skip_stmts = Vec::new();
    // Ordinal chunk tags, assigned to nested components in declaration order.
    // Tag 0 is reserved, so the first component is 1.
    let mut next_tag: u16 = 1;

    for field in fields {
        let ident = field.ident.as_ref().expect("named field");

        let force_elements = has_save_elements(&field.attrs);

        match parse_save_skip(&field.attrs) {
            SaveSkip::None => {
                // Normal field: generate save + load code based on type
                let (save, load) = gen_field_io(ident, &field.ty, force_elements);
                let (save, load) = if delegates_to_saveable(&field.ty) {
                    // Parents frame children: a nested component goes in a
                    // chunk so a change to it cannot walk into its siblings.
                    let tag = next_tag;
                    next_tag = next_tag
                        .checked_add(1)
                        .filter(|t| *t != u16::MAX)
                        .unwrap_or_else(|| {
                            panic!("{struct_name} has too many nested components for u16 tags")
                        });
                    let path = format!("{struct_name}.{ident}");
                    (
                        quote! { w.write_tlv(#tag, |w| { #save }); },
                        quote! {
                            r.read_component(#tag, #path, |r| { #load Ok(()) })?;
                        },
                    )
                } else {
                    (save, load)
                };
                save_stmts.push(save);
                load_stmts.push(load);
            }
            SaveSkip::Keep => {
                // #[save_skip] — excluded, no code generated
            }
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

    let expanded = quote! {
        impl phosphor_core::prelude::Saveable for #struct_name {
            fn save_state(&self, w: &mut phosphor_core::prelude::StateWriter) {
                #version_write
                #(#save_stmts)*
            }

            fn load_state(
                &mut self,
                r: &mut phosphor_core::prelude::StateReader,
            ) -> Result<(), phosphor_core::prelude::SaveError> {
                #version_read
                #(#load_stmts)*
                #(#load_skip_stmts)*
                Ok(())
            }
        }
    };

    TokenStream::from(expanded)
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

/// Check if a field has the `#[save_elements]` attribute.
fn has_save_elements(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("save_elements"))
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
        // An array delegates only when its elements do; `[u8; N]` and
        // `[u16; N]` are inline either way, `#[save_elements]` or not.
        Type::Array(arr) => !is_primitive_type(&arr.elem),
        Type::Path(path) => {
            if is_primitive_type(ty) {
                return false;
            }
            // Vec<u8> is a length-prefixed blob. A Vec of anything else is
            // rejected by `gen_field_io`, so it never reaches a chunk.
            path.path.segments.last().expect("non-empty path").ident != "Vec"
        }
        _ => true,
    }
}

/// Generate save and load token streams for a single field based on its type.
fn gen_field_io(
    ident: &syn::Ident,
    ty: &Type,
    force_elements: bool,
) -> (TokenStream2, TokenStream2) {
    match ty {
        // Fixed-size array: [T; N]
        Type::Array(arr) => gen_array_io(ident, &arr.elem, force_elements),
        // Path types: primitives, Vec<u8>, or Saveable delegates
        Type::Path(path) => {
            let seg = path.path.segments.last().expect("non-empty path");
            let type_name = seg.ident.to_string();
            match type_name.as_str() {
                "u8" => (
                    quote! { w.write_u8(self.#ident); },
                    quote! { self.#ident = r.read_u8()?; },
                ),
                "u16" => (
                    quote! { w.write_u16_le(self.#ident); },
                    quote! { self.#ident = r.read_u16_le()?; },
                ),
                "u32" => (
                    quote! { w.write_u32_le(self.#ident); },
                    quote! { self.#ident = r.read_u32_le()?; },
                ),
                "u64" => (
                    quote! { w.write_u64_le(self.#ident); },
                    quote! { self.#ident = r.read_u64_le()?; },
                ),
                "i16" => (
                    quote! { w.write_i16_le(self.#ident); },
                    quote! { self.#ident = r.read_i16_le()?; },
                ),
                "i32" => (
                    quote! { w.write_i32_le(self.#ident); },
                    quote! { self.#ident = r.read_i32_le()?; },
                ),
                "i64" => (
                    quote! { w.write_i64_le(self.#ident); },
                    quote! { self.#ident = r.read_i64_le()?; },
                ),
                "f32" => (
                    quote! { w.write_f32_le(self.#ident); },
                    quote! { self.#ident = r.read_f32_le()?; },
                ),
                "f64" => (
                    quote! { w.write_f64_le(self.#ident); },
                    quote! { self.#ident = r.read_f64_le()?; },
                ),
                "bool" => (
                    quote! { w.write_bool(self.#ident); },
                    quote! { self.#ident = r.read_bool()?; },
                ),
                "Vec" => {
                    // Verify it's Vec<u8>
                    if is_vec_u8(seg) {
                        (
                            quote! { w.write_bytes(&self.#ident); },
                            quote! { self.#ident = r.read_bytes()?.to_vec(); },
                        )
                    } else {
                        panic!(
                            "Saveable derive only supports Vec<u8>; field `{}` has unsupported Vec type",
                            ident
                        );
                    }
                }
                // Unknown type — delegate to Saveable
                _ => (
                    quote! { phosphor_core::prelude::Saveable::save_state(&self.#ident, w); },
                    quote! { phosphor_core::prelude::Saveable::load_state(&mut self.#ident, r)?; },
                ),
            }
        }
        _ => {
            // Fallback: delegate to Saveable
            (
                quote! { phosphor_core::prelude::Saveable::save_state(&self.#ident, w); },
                quote! { phosphor_core::prelude::Saveable::load_state(&mut self.#ident, r)?; },
            )
        }
    }
}

/// Generate save/load for `[T; N]` arrays.
///
/// When `force_elements` is true, `[u8; N]` is serialized per-element instead
/// of using the bulk `write_bytes`/`read_bytes_into` path. This preserves
/// compatibility with hand-written impls that used individual `write_u8` calls.
fn gen_array_io(
    ident: &syn::Ident,
    elem_ty: &Type,
    force_elements: bool,
) -> (TokenStream2, TokenStream2) {
    // Check if element is u8 — use bulk bytes path (unless force_elements)
    if is_type_u8(elem_ty) && !force_elements {
        return (
            quote! { w.write_bytes(&self.#ident); },
            quote! { r.read_bytes_into(&mut self.#ident)?; },
        );
    }

    // For other element types, generate a loop
    let (elem_save, elem_load) = gen_array_element_io(ident, elem_ty);
    (
        quote! { for __v in &self.#ident { #elem_save } },
        quote! { for __v in &mut self.#ident { #elem_load } },
    )
}

/// Generate per-element save/load for array loops.
fn gen_array_element_io(ident: &syn::Ident, elem_ty: &Type) -> (TokenStream2, TokenStream2) {
    if let Type::Path(path) = elem_ty {
        let seg = path.path.segments.last().expect("non-empty path");
        let type_name = seg.ident.to_string();
        match type_name.as_str() {
            "u8" => (
                quote! { w.write_u8(*__v); },
                quote! { *__v = r.read_u8()?; },
            ),
            "u16" => (
                quote! { w.write_u16_le(*__v); },
                quote! { *__v = r.read_u16_le()?; },
            ),
            "u32" => (
                quote! { w.write_u32_le(*__v); },
                quote! { *__v = r.read_u32_le()?; },
            ),
            "u64" => (
                quote! { w.write_u64_le(*__v); },
                quote! { *__v = r.read_u64_le()?; },
            ),
            "i16" => (
                quote! { w.write_i16_le(*__v); },
                quote! { *__v = r.read_i16_le()?; },
            ),
            "i32" => (
                quote! { w.write_i32_le(*__v); },
                quote! { *__v = r.read_i32_le()?; },
            ),
            "i64" => (
                quote! { w.write_i64_le(*__v); },
                quote! { *__v = r.read_i64_le()?; },
            ),
            "f32" => (
                quote! { w.write_f32_le(*__v); },
                quote! { *__v = r.read_f32_le()?; },
            ),
            "f64" => (
                quote! { w.write_f64_le(*__v); },
                quote! { *__v = r.read_f64_le()?; },
            ),
            "bool" => (
                quote! { w.write_bool(*__v); },
                quote! { *__v = r.read_bool()?; },
            ),
            _ => {
                // Delegate to nested Saveable
                let _ = ident; // suppress unused warning
                (
                    quote! { phosphor_core::prelude::Saveable::save_state(__v, w); },
                    quote! { phosphor_core::prelude::Saveable::load_state(__v, r)?; },
                )
            }
        }
    } else {
        // Non-path element type — delegate to Saveable
        (
            quote! { phosphor_core::prelude::Saveable::save_state(__v, w); },
            quote! { phosphor_core::prelude::Saveable::load_state(__v, r)?; },
        )
    }
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
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return is_type_u8(inner);
    }
    false
}
