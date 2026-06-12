use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DeriveInput, Expr, Fields, Type, parse_macro_input};

/// Derive macro that generates a `BusDebug` implementation for a struct.
///
/// Annotate fields with:
/// - `#[debug_device("Name")]` — field implements `Device`, listed in `devices()`.
///   Also generates `write_device_register()` and `reset_device()` dispatch.
/// - `#[debug_cpu("Name")]` — field implements `DebugCpu`, listed in both `devices()`
///   AND `cpus()`. Debug reads/writes are auto-routed through the matching
///   `#[debug_map(cpu = N)]` field's `MemoryMap::debug_read`/`debug_write`.
/// - `#[debug_cpu("Name", read = "method", write = "method")]` — explicit version:
///   names `&self` / `&mut self` methods on the struct for side-effect-free memory access.
/// - `#[debug_map(cpu = N)]` — field is a `MemoryMap` linked to CPU index N.
///   Generates watchpoint routing, `peek` (backed/I/O/unmapped semantics via
///   `AddressSpace16::debug_peek`), and (when linked to a `#[debug_cpu]`)
///   debug memory access.
///
/// CPU index assignment is positional: first `#[debug_cpu]` is index 0, etc.
/// Device indices for `write_device_register` / `reset_device` match `devices()` order.
#[proc_macro_derive(BusDebug, attributes(debug_device, debug_cpu, debug_map))]
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

    let mut device_entries = Vec::new(); // (name, field_ident, is_device) for all annotated fields
    let mut cpu_entries: Vec<(
        syn::LitStr,
        syn::Ident,
        Option<syn::LitStr>,
        Option<syn::LitStr>,
    )> = Vec::new(); // (name, field_ident, read_method?, write_method?)
    let mut map_entries: Vec<MapEntry> = Vec::new(); // (cpu_index, field_ident) for MemoryMap fields

    for field in fields {
        let field_ident = field.ident.as_ref().expect("named field");

        for attr in &field.attrs {
            if attr.path().is_ident("debug_device") {
                // #[debug_device("Name")] — field implements Device
                let name: syn::LitStr = attr
                    .parse_args()
                    .expect("debug_device expects a string literal: #[debug_device(\"Name\")]");
                device_entries.push((name, field_ident.clone(), true));
            } else if attr.path().is_ident("debug_cpu") {
                // #[debug_cpu("Name")] or #[debug_cpu("Name", read = "method", write = "method")]
                let args: CpuArgs = attr
                    .parse_args()
                    .expect("debug_cpu expects: (\"Name\") or (\"Name\", read = \"method\", write = \"method\")");
                // CPUs appear in both devices() and cpus()
                device_entries.push((args.name.clone(), field_ident.clone(), false));
                cpu_entries.push((args.name, field_ident.clone(), args.read, args.write));
            } else if attr.path().is_ident("debug_map") {
                // #[debug_map(cpu = N)] — field is a MemoryMap linked to CPU index N
                let args: MapArgs = attr.parse_args().expect("debug_map expects: (cpu = N)");
                map_entries.push(MapEntry {
                    cpu_index: args.cpu_index,
                    field_ident: field_ident.clone(),
                });
            }
        }
    }

    // Generate devices() body
    let device_items = device_entries.iter().map(|(name, ident, _)| {
        quote! { (#name, &self.#ident as &dyn phosphor_core::core::debug::Debuggable) }
    });

    // Generate cpus() body
    let cpu_items = cpu_entries.iter().map(|(name, ident, _, _)| {
        quote! { (#name, &self.#ident as &dyn phosphor_core::core::debug::DebugCpu) }
    });

    // Generate read() match arms
    let read_arms: Vec<_> = cpu_entries
        .iter()
        .enumerate()
        .map(|(i, (_, _, read_method, _))| {
            let idx = i;
            if let Some(read_method) = read_method {
                // Explicit method: self.method(addr)
                let read_ident =
                    syn::Ident::new(read_method.value().as_str(), read_method.span());
                quote! { #idx => self.#read_ident(addr) }
            } else {
                // Auto-route through matching #[debug_map(cpu = N)]
                let map_field = map_entries
                    .iter()
                    .find(|m| m.cpu_index == i)
                    .unwrap_or_else(|| {
                        panic!(
                            "debug_cpu at index {i} has no read method and no matching #[debug_map(cpu = {i})]"
                        )
                    });
                let map_ident = &map_field.field_ident;
                quote! { #idx => self.#map_ident.debug_read(addr) }
            }
        })
        .collect();

    // Generate write() match arms
    let write_arms: Vec<_> = cpu_entries
        .iter()
        .enumerate()
        .map(|(i, (_, _, _, write_method))| {
            let idx = i;
            if let Some(write_method) = write_method {
                // Explicit method: self.method(addr, data)
                let write_ident =
                    syn::Ident::new(write_method.value().as_str(), write_method.span());
                quote! { #idx => self.#write_ident(addr, data) }
            } else {
                // Auto-route through matching #[debug_map(cpu = N)]
                let map_field = map_entries
                    .iter()
                    .find(|m| m.cpu_index == i)
                    .unwrap_or_else(|| {
                        panic!(
                            "debug_cpu at index {i} has no write method and no matching #[debug_map(cpu = {i})]"
                        )
                    });
                let map_ident = &map_field.field_ident;
                quote! { #idx => self.#map_ident.debug_write(addr, data) }
            }
        })
        .collect();

    // Generate write_device_register() match arms (only #[debug_device] fields)
    let device_write_arms =
        device_entries
            .iter()
            .enumerate()
            .filter_map(|(i, (_, ident, is_device))| {
                if *is_device {
                    let idx = i;
                    Some(quote! {
                        #idx => phosphor_core::device::Device::write(&mut self.#ident, offset, data)
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
            .filter_map(|(i, (_, ident, is_device))| {
                if *is_device {
                    let idx = i;
                    Some(quote! {
                        #idx => phosphor_core::device::Device::reset(&mut self.#ident)
                    })
                } else {
                    None
                }
            });

    // Generate watchpoint methods (only when #[debug_map] fields exist)
    let watchpoint_methods = if !map_entries.is_empty() {
        // take_watchpoint_hit: chain .or_else() across all maps (declaration order)
        let take_hit_chain = map_entries.iter().map(|entry| {
            let ident = &entry.field_ident;
            quote! { .or_else(|| self.#ident.take_hit()) }
        });

        // set_watchpoint / clear_watchpoint: match on cpu_index, passing it
        // through so hits record which CPU's address space fired
        let set_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            quote! { #idx => self.#ident.set_watchpoint(cpu_index, addr, kind) }
        });
        let clear_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            quote! { #idx => self.#ident.clear_watchpoint(cpu_index, addr, kind) }
        });

        // clear_all_watchpoints: call on every map
        let clear_all_calls = map_entries.iter().map(|entry| {
            let ident = &entry.field_ident;
            quote! { self.#ident.clear_all_watchpoints(); }
        });

        // memory_map: match on cpu_index
        let map_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            quote! { #idx => Some(&self.#ident) }
        });

        // peek: per-map backed/io/unmapped semantics; CPUs without a map
        // fall back to the read()-based default semantics
        let peek_arms = map_entries.iter().map(|entry| {
            let idx = entry.cpu_index;
            let ident = &entry.field_ident;
            quote! { #idx => self.#ident.debug_peek(addr as u16) }
        });

        quote! {
            fn peek(&self, cpu_index: usize, addr: u32) -> phosphor_core::core::memory_map::DebugRead {
                if addr > 0xFFFF {
                    return phosphor_core::core::memory_map::DebugRead::Unmapped;
                }
                match cpu_index {
                    #(#peek_arms,)*
                    _ => match self.read(cpu_index, addr) {
                        Some(value) => phosphor_core::core::memory_map::DebugRead::Backed {
                            value: value as u32,
                            width: 1,
                            region_id: 0,
                        },
                        None => phosphor_core::core::memory_map::DebugRead::Unmapped,
                    },
                }
            }

            fn take_watchpoint_hit(&mut self) -> Option<phosphor_core::core::memory_map::WatchpointHit> {
                None #(#take_hit_chain)*
            }

            fn set_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: phosphor_core::core::memory_map::WatchpointKind) {
                // 16-bit maps: addresses above 0xFFFF can never fire.
                let Ok(addr) = u16::try_from(addr) else {
                    return;
                };
                match cpu_index {
                    #(#set_arms,)*
                    _ => {}
                }
            }

            fn clear_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: phosphor_core::core::memory_map::WatchpointKind) {
                let Ok(addr) = u16::try_from(addr) else {
                    return;
                };
                match cpu_index {
                    #(#clear_arms,)*
                    _ => {}
                }
            }

            fn clear_all_watchpoints(&mut self) {
                #(#clear_all_calls)*
            }

            fn memory_map(&self, cpu_index: usize) -> Option<&phosphor_core::core::memory_map::MemoryMap> {
                match cpu_index {
                    #(#map_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl phosphor_core::core::debug::BusDebug for #struct_name {
            fn devices(&self) -> Vec<(&str, &dyn phosphor_core::core::debug::Debuggable)> {
                vec![#(#device_items),*]
            }

            fn cpus(&self) -> Vec<(&str, &dyn phosphor_core::core::debug::DebugCpu)> {
                vec![#(#cpu_items),*]
            }

            fn read(&self, cpu_index: usize, addr: u32) -> Option<u8> {
                // Board maps and read methods are 16-bit; higher addresses
                // are unmapped.
                let addr = u16::try_from(addr).ok()?;
                match cpu_index {
                    #(#read_arms,)*
                    _ => None,
                }
            }

            fn write(&mut self, cpu_index: usize, addr: u32, data: u8) {
                let Ok(addr) = u16::try_from(addr) else {
                    return;
                };
                match cpu_index {
                    #(#write_arms,)*
                    _ => {}
                }
            }

            fn write_device_register(&mut self, device_index: usize, offset: u16, data: u8) {
                match device_index {
                    #(#device_write_arms,)*
                    _ => {}
                }
            }

            fn reset_device(&mut self, device_index: usize) {
                match device_index {
                    #(#device_reset_arms,)*
                    _ => {}
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

    for field in fields {
        let ident = field.ident.as_ref().expect("named field");

        let force_elements = has_save_elements(&field.attrs);

        match parse_save_skip(&field.attrs) {
            SaveSkip::None => {
                // Normal field: generate save + load code based on type
                let (save, load) = gen_field_io(ident, &field.ty, force_elements);
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
