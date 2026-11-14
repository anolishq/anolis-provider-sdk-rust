//! What a provider declares: device types as data, and the values, readings,
//! errors and health it hands back.
//!
//! A device type is declared once. The SDK derives everything ADPP needs from
//! that one declaration (function ids, the default signal set, argument
//! validation), so the capabilities a client sees and the checks the SDK applies
//! cannot drift apart. A declaration that breaks a protocol rule (a signal id
//! that is not snake_case, a bound on a string, two functions with one name) is
//! a bug in the provider, and panics at the line that declares it.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use crate::proto;

/// The type of a signal, argument or result (ADPP `ValueType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    Bool,
    Int64,
    Uint64,
    Double,
    String,
    Bytes,
}

impl ValueType {
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Int64 | Self::Uint64 | Self::Double)
    }

    pub(crate) fn to_proto(self) -> proto::ValueType {
        match self {
            Self::Bool => proto::ValueType::Bool,
            Self::Int64 => proto::ValueType::Int64,
            Self::Uint64 => proto::ValueType::Uint64,
            Self::Double => proto::ValueType::Double,
            Self::String => proto::ValueType::String,
            Self::Bytes => proto::ValueType::Bytes,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bool => "BOOL",
            Self::Int64 => "INT64",
            Self::Uint64 => "UINT64",
            Self::Double => "DOUBLE",
            Self::String => "STRING",
            Self::Bytes => "BYTES",
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A signal value, argument or result.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int64(i64),
    Uint64(u64),
    Double(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::Bool(_) => ValueType::Bool,
            Self::Int64(_) => ValueType::Int64,
            Self::Uint64(_) => ValueType::Uint64,
            Self::Double(_) => ValueType::Double,
            Self::String(_) => ValueType::String,
            Self::Bytes(_) => ValueType::Bytes,
        }
    }

    /// This value as the declared type: an integer that fits converts between
    /// INT64 and UINT64, and any integer converts to DOUBLE. Anything else of
    /// the wrong type is refused.
    pub(crate) fn coerce(self, to: ValueType) -> Result<Value, String> {
        let converted = match (self, to) {
            (v, t) if v.value_type() == t => Some(v),
            (Value::Int64(i), ValueType::Uint64) => u64::try_from(i).ok().map(Value::Uint64),
            (Value::Uint64(u), ValueType::Int64) => i64::try_from(u).ok().map(Value::Int64),
            (Value::Int64(i), ValueType::Double) => Some(Value::Double(i as f64)),
            (Value::Uint64(u), ValueType::Double) => Some(Value::Double(u as f64)),
            (v, _) => return Err(format!("{v} is not a valid {to}")),
        };
        converted.ok_or_else(|| format!("the value does not fit {to}"))
    }

    pub(crate) fn to_proto(&self) -> proto::Value {
        use proto::value::Kind;
        let kind = match self {
            Self::Bool(b) => Kind::BoolValue(*b),
            Self::Int64(i) => Kind::Int64Value(*i),
            Self::Uint64(u) => Kind::Uint64Value(*u),
            Self::Double(d) => Kind::DoubleValue(*d),
            Self::String(s) => Kind::StringValue(s.clone()),
            Self::Bytes(b) => Kind::BytesValue(b.clone()),
        };
        proto::Value { r#type: self.value_type().to_proto() as i32, kind: Some(kind) }
    }

    /// The value a request carries, which must be of the declared type.
    pub(crate) fn from_proto(want: ValueType, value: &proto::Value) -> Result<Value, String> {
        use proto::value::Kind;
        let tagged = proto::ValueType::try_from(value.r#type).ok();
        if tagged != Some(proto::ValueType::Unspecified) && tagged != Some(want.to_proto()) {
            let got = tagged.map_or_else(|| value.r#type.to_string(), |t| t.as_str_name().to_owned());
            return Err(format!("expects {want}, got {got}"));
        }
        let parsed = match (&value.kind, want) {
            (Some(Kind::BoolValue(b)), ValueType::Bool) => Value::Bool(*b),
            (Some(Kind::Int64Value(i)), ValueType::Int64) => Value::Int64(*i),
            (Some(Kind::Uint64Value(u)), ValueType::Uint64) => Value::Uint64(*u),
            (Some(Kind::DoubleValue(d)), ValueType::Double) => Value::Double(*d),
            (Some(Kind::StringValue(s)), ValueType::String) => Value::String(s.clone()),
            (Some(Kind::BytesValue(b)), ValueType::Bytes) => Value::Bytes(b.clone()),
            (None, _) => return Err(format!("expects {want}, got an empty value")),
            (Some(_), _) => return Err(format!("expects {want}, got a value of another type")),
        };
        Ok(parsed)
    }

    /// Order two values of the same numeric type; `None` for anything else.
    fn compare(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int64(a), Value::Int64(b)) => Some(a.cmp(b)),
            (Value::Uint64(a), Value::Uint64(b)) => Some(a.cmp(b)),
            (Value::Double(a), Value::Double(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int64(i) => write!(f, "{i}"),
            Self::Uint64(u) => write!(f, "{u}"),
            Self::Double(d) => write!(f, "{d}"),
            Self::String(s) => write!(f, "{s:?}"),
            Self::Bytes(b) => write!(f, "{} bytes", b.len()),
        }
    }
}

macro_rules! value_from {
    ($($t:ty => $variant:ident as $as:ty),* $(,)?) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::$variant(<$as>::from(v))
            }
        }
    )*};
}

value_from! {
    bool => Bool as bool,
    i8 => Int64 as i64, i16 => Int64 as i64, i32 => Int64 as i64, i64 => Int64 as i64,
    u8 => Uint64 as u64, u16 => Uint64 as u64, u32 => Uint64 as u64, u64 => Uint64 as u64,
    f32 => Double as f64, f64 => Double as f64,
    String => String as String, &str => String as String,
    Vec<u8> => Bytes as Vec<u8>, &[u8] => Bytes as Vec<u8>,
}

/// What a call does, for the runtime's mode policy (ADPP `FunctionPolicy.Category`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Unspecified,
    Read,
    Config,
    Actuate,
}

impl Category {
    fn to_proto(self) -> proto::function_policy::Category {
        use proto::function_policy::Category as C;
        match self {
            Self::Unspecified => C::Unspecified,
            Self::Read => C::Read,
            Self::Config => C::Config,
            Self::Actuate => C::Actuate,
        }
    }
}

fn is_snake_case(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A function argument or result (ADPP `ArgSpec`). Bounds are inclusive.
#[derive(Clone, Debug)]
pub struct Arg {
    name: String,
    value_type: ValueType,
    required: bool,
    min: Option<Value>,
    max: Option<Value>,
    unit: String,
    description: String,
    allowed_values: Vec<String>,
}

impl Arg {
    /// A required argument (or a result) of the given type.
    ///
    /// # Panics
    ///
    /// If `name` is empty.
    #[track_caller]
    pub fn new(name: impl Into<String>, value_type: ValueType) -> Self {
        let name = name.into();
        assert!(!name.is_empty(), "an argument needs a name");
        Self {
            name,
            value_type,
            required: true,
            min: None,
            max: None,
            unit: String::new(),
            description: String::new(),
            allowed_values: Vec::new(),
        }
    }

    #[track_caller]
    pub fn bool(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::Bool)
    }
    #[track_caller]
    pub fn int64(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::Int64)
    }
    #[track_caller]
    pub fn uint64(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::Uint64)
    }
    #[track_caller]
    pub fn double(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::Double)
    }
    #[track_caller]
    pub fn string(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::String)
    }
    #[track_caller]
    pub fn bytes(name: impl Into<String>) -> Self {
        Self::new(name, ValueType::Bytes)
    }

    /// The argument may be left out of a call.
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// The inclusive lower bound.
    ///
    /// # Panics
    ///
    /// If the argument is not numeric, or the bound is not finite, does not
    /// fit the argument's type, or is above the upper bound.
    #[track_caller]
    pub fn min(mut self, bound: impl Into<Value>) -> Self {
        self.min = Some(self.bound(bound.into()));
        self.check_order();
        self
    }

    /// The inclusive upper bound. Panics as [`Arg::min`] does.
    #[track_caller]
    pub fn max(mut self, bound: impl Into<Value>) -> Self {
        self.max = Some(self.bound(bound.into()));
        self.check_order();
        self
    }

    /// Both inclusive bounds. Panics as [`Arg::min`] does.
    #[track_caller]
    pub fn range(self, min: impl Into<Value>, max: impl Into<Value>) -> Self {
        self.min(min).max(max)
    }

    /// The values a STRING argument may take.
    ///
    /// # Panics
    ///
    /// If the argument is not a STRING.
    #[track_caller]
    pub fn allowed_values<I: IntoIterator<Item = S>, S: Into<String>>(mut self, values: I) -> Self {
        assert!(
            self.value_type == ValueType::String,
            "argument {:?}: allowed_values apply only to STRING arguments",
            self.name
        );
        self.allowed_values = values.into_iter().map(Into::into).collect();
        self
    }

    #[track_caller]
    fn bound(&self, bound: Value) -> Value {
        assert!(self.value_type.is_numeric(), "argument {:?}: bounds apply only to numeric types", self.name);
        if let Value::Double(d) = bound {
            assert!(d.is_finite(), "argument {:?}: bounds must be finite", self.name);
        }
        let bound_display = bound.to_string();
        bound.coerce(self.value_type).unwrap_or_else(|_| {
            panic!("argument {:?}: bound {bound_display} does not fit {}", self.name, self.value_type)
        })
    }

    #[track_caller]
    fn check_order(&self) {
        if let (Some(min), Some(max)) = (&self.min, &self.max) {
            assert!(
                min.compare(max) != Some(std::cmp::Ordering::Greater),
                "argument {:?}: min {min} is above max {max}",
                self.name
            );
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn value_type(&self) -> ValueType {
        self.value_type
    }
    pub fn is_required(&self) -> bool {
        self.required
    }

    /// semantics.md §8.3 for one supplied value: type, finiteness, then the
    /// inclusive bounds, then the allowed values.
    pub(crate) fn validate(&self, raw: &proto::Value) -> Result<Value, crate::ProviderError> {
        use crate::{ProviderError, StatusCode};
        let value = Value::from_proto(self.value_type, raw).map_err(|err| {
            ProviderError::new(StatusCode::InvalidArgument, format!("argument {:?} {err}", self.name))
        })?;
        if let Value::Double(d) = value
            && !d.is_finite()
        {
            return Err(ProviderError::new(
                StatusCode::InvalidArgument,
                format!("argument {:?} must be finite, got {d}", self.name),
            ));
        }
        let below = self.min.as_ref().is_some_and(|min| value.compare(min) == Some(std::cmp::Ordering::Less));
        let above = self.max.as_ref().is_some_and(|max| value.compare(max) == Some(std::cmp::Ordering::Greater));
        if below || above {
            let show = |b: &Option<Value>| b.as_ref().map_or_else(|| "-".to_owned(), ToString::to_string);
            return Err(ProviderError::new(
                StatusCode::OutOfRange,
                format!("argument {:?} = {value} is outside [{}, {}]", self.name, show(&self.min), show(&self.max)),
            ));
        }
        if let Value::String(s) = &value
            && !self.allowed_values.is_empty()
            && !self.allowed_values.contains(s)
        {
            return Err(ProviderError::new(
                StatusCode::InvalidArgument,
                format!("argument {:?} must be one of {:?}", self.name, self.allowed_values),
            ));
        }
        Ok(value)
    }

    pub(crate) fn to_proto(&self) -> proto::ArgSpec {
        let mut spec = proto::ArgSpec {
            name: self.name.clone(),
            r#type: self.value_type.to_proto() as i32,
            unit: self.unit.clone(),
            description: self.description.clone(),
            required: self.required,
            allowed_values: self.allowed_values.clone(),
            ..Default::default()
        };
        for (bound, is_min) in [(&self.min, true), (&self.max, false)] {
            match (bound, is_min) {
                (Some(Value::Double(d)), true) => spec.min_double = Some(*d),
                (Some(Value::Double(d)), false) => spec.max_double = Some(*d),
                (Some(Value::Int64(i)), true) => spec.min_int64 = Some(*i),
                (Some(Value::Int64(i)), false) => spec.max_int64 = Some(*i),
                (Some(Value::Uint64(u)), true) => spec.min_uint64 = Some(*u),
                (Some(Value::Uint64(u)), false) => spec.max_uint64 = Some(*u),
                _ => {}
            }
        }
        spec
    }
}

/// A readable value (ADPP `SignalSpec`).
///
/// A signal is a default signal unless [`Signal::not_default`] says otherwise:
/// default signals are what a read that names no signals returns
/// (semantics.md §7.2). `poll_hint_hz` above 0, 1.0 unless set, is what makes
/// the Anolis runtime poll the signal; 0 means it is never polled.
#[derive(Clone, Debug)]
pub struct Signal {
    id: String,
    value_type: ValueType,
    name: String,
    unit: String,
    description: String,
    default: bool,
    poll_hint_hz: f64,
    stale_after_ms: u32,
}

impl Signal {
    /// # Panics
    ///
    /// If `id` is not snake_case (`[a-z][a-z0-9_]*`, executable profile §5).
    #[track_caller]
    pub fn new(id: impl Into<String>, value_type: ValueType) -> Self {
        let id = id.into();
        assert!(is_snake_case(&id), "signal id {id:?} must be snake_case ([a-z][a-z0-9_]*)");
        Self {
            id,
            value_type,
            name: String::new(),
            unit: String::new(),
            description: String::new(),
            default: true,
            poll_hint_hz: 1.0,
            stale_after_ms: 0,
        }
    }

    /// A human-readable name; the id when unset.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Leave the signal out of a read that names no signals.
    pub fn not_default(mut self) -> Self {
        self.default = false;
        self
    }

    /// # Panics
    ///
    /// If `hz` is negative or not finite.
    #[track_caller]
    pub fn poll_hint_hz(mut self, hz: f64) -> Self {
        assert!(hz.is_finite() && hz >= 0.0, "signal {:?}: poll_hint_hz must be finite and not negative", self.id);
        self.poll_hint_hz = hz;
        self
    }

    /// A reading older than this is reported STALE. 0, the default, never is.
    pub fn stale_after_ms(mut self, ms: u32) -> Self {
        self.stale_after_ms = ms;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn value_type(&self) -> ValueType {
        self.value_type
    }
    pub fn is_default(&self) -> bool {
        self.default
    }
    pub(crate) fn stale_after(&self) -> u32 {
        self.stale_after_ms
    }

    pub(crate) fn to_proto(&self) -> proto::SignalSpec {
        proto::SignalSpec {
            signal_id: self.id.clone(),
            name: if self.name.is_empty() { self.id.clone() } else { self.name.clone() },
            description: self.description.clone(),
            value_type: self.value_type.to_proto() as i32,
            unit: self.unit.clone(),
            poll_hint_hz: self.poll_hint_hz,
            stale_after_ms: self.stale_after_ms,
        }
    }
}

/// A callable operation (ADPP `FunctionSpec`). Its id is its 1-based position
/// in its device type's functions (executable profile §5).
#[derive(Clone, Debug)]
pub struct Function {
    id: u32,
    name: String,
    args: Vec<Arg>,
    results: Vec<Arg>,
    description: String,
    category: Category,
    allowed_modes: Vec<String>,
    min_interval_ms: u32,
    idempotent: bool,
}

impl Function {
    /// A function in the ACTUATE category with no arguments and no results.
    ///
    /// # Panics
    ///
    /// If `name` is empty.
    #[track_caller]
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(!name.is_empty(), "a function needs a name");
        Self {
            id: 0,
            name,
            args: Vec::new(),
            results: Vec::new(),
            description: String::new(),
            category: Category::Actuate,
            allowed_modes: Vec::new(),
            min_interval_ms: 0,
            idempotent: false,
        }
    }

    /// # Panics
    ///
    /// If the function already has an argument of that name.
    #[track_caller]
    pub fn arg(mut self, arg: Arg) -> Self {
        assert!(self.args.iter().all(|a| a.name != arg.name), "duplicate argument {:?} of {:?}", arg.name, self.name);
        self.args.push(arg);
        self
    }

    /// A declared result: a completed call must return it (semantics.md §8.1).
    ///
    /// # Panics
    ///
    /// If the function already has a result of that name.
    #[track_caller]
    pub fn result(mut self, result: Arg) -> Self {
        assert!(
            self.results.iter().all(|r| r.name != result.name),
            "duplicate result {:?} of {:?}",
            result.name,
            self.name
        );
        self.results.push(result);
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn category(mut self, category: Category) -> Self {
        self.category = category;
        self
    }

    /// Modes the runtime allows the call in; none means every mode.
    pub fn allowed_modes<I: IntoIterator<Item = S>, S: Into<String>>(mut self, modes: I) -> Self {
        self.allowed_modes = modes.into_iter().map(Into::into).collect();
        self
    }

    pub fn min_interval_ms(mut self, ms: u32) -> Self {
        self.min_interval_ms = ms;
        self
    }

    pub fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
    }

    /// The function's id within its device type; 0 until the type declares it.
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn args(&self) -> &[Arg] {
        &self.args
    }
    pub fn results(&self) -> &[Arg] {
        &self.results
    }

    pub(crate) fn to_proto(&self) -> proto::FunctionSpec {
        proto::FunctionSpec {
            function_id: self.id,
            name: self.name.clone(),
            description: self.description.clone(),
            policy: Some(proto::FunctionPolicy {
                category: self.category.to_proto() as i32,
                allowed_modes: self.allowed_modes.clone(),
                min_interval_ms: self.min_interval_ms,
                is_idempotent: self.idempotent,
                ..Default::default()
            }),
            args: self.args.iter().map(Arg::to_proto).collect(),
            results: self.results.iter().map(Arg::to_proto).collect(),
        }
    }
}

/// A kind of device: every device of one type has the same capabilities.
/// Declare it with [`DeviceType::new`] and finish it with [`DeviceType::build`].
#[derive(Clone, Debug)]
pub struct DeviceType {
    id: String,
    version: String,
    signals: Vec<Signal>,
    functions: Vec<Function>,
}

impl DeviceType {
    /// # Panics
    ///
    /// If `id` is empty.
    #[track_caller]
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        assert!(!id.is_empty(), "a device type needs an id");
        Self { id, version: "1".to_owned(), signals: Vec::new(), functions: Vec::new() }
    }

    /// The type's version, reported in the device's `type_version`; "1" unless set.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// # Panics
    ///
    /// If the type already has a signal with that id.
    #[track_caller]
    pub fn signal(mut self, signal: Signal) -> Self {
        assert!(self.signals.iter().all(|s| s.id != signal.id), "duplicate signal {:?} in {:?}", signal.id, self.id);
        self.signals.push(signal);
        self
    }

    /// Declare the next function; it gets the next id, counting from 1.
    ///
    /// # Panics
    ///
    /// If the type already has a function with that name.
    #[track_caller]
    pub fn function(mut self, mut function: Function) -> Self {
        assert!(
            self.functions.iter().all(|f| f.name != function.name),
            "duplicate function {:?} in {:?}",
            function.name,
            self.id
        );
        function.id = u32::try_from(self.functions.len() + 1).expect("fewer than 2^32 functions");
        self.functions.push(function);
        self
    }

    /// The finished type, shared by the devices of that type.
    ///
    /// # Panics
    ///
    /// If the type declares signals but none of them is a default signal
    /// (semantics.md §7.2).
    #[track_caller]
    pub fn build(self) -> Arc<DeviceType> {
        assert!(
            self.signals.is_empty() || self.signals.iter().any(|s| s.default),
            "device type {:?} declares signals but no default signal (semantics.md §7.2)",
            self.id
        );
        Arc::new(self)
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn signal_by_id(&self, id: &str) -> Option<&Signal> {
        self.signals.iter().find(|s| s.id == id)
    }

    pub fn function_by_id(&self, id: u32) -> Option<&Function> {
        let index = usize::try_from(id).ok()?.checked_sub(1)?;
        self.functions.get(index)
    }

    pub fn function_by_name(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.name == name)
    }

    pub fn default_signal_ids(&self) -> Vec<&str> {
        self.signals.iter().filter(|s| s.default).map(|s| s.id.as_str()).collect()
    }

    pub(crate) fn capabilities(&self) -> proto::CapabilitySet {
        proto::CapabilitySet {
            functions: self.functions.iter().map(Function::to_proto).collect(),
            signals: self.signals.iter().map(Signal::to_proto).collect(),
        }
    }
}

/// One device instance. Its id must stay stable for the provider process.
#[derive(Clone, Debug)]
pub struct Device {
    id: String,
    device_type: Arc<DeviceType>,
    label: String,
    address: String,
    tags: HashMap<String, String>,
}

impl Device {
    pub fn new(id: impl Into<String>, device_type: &Arc<DeviceType>) -> Self {
        Self {
            id: id.into(),
            device_type: Arc::clone(device_type),
            label: String::new(),
            address: String::new(),
            tags: HashMap::new(),
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = address.into();
        self
    }

    pub fn tag(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.tags.insert(key.into(), value.into());
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn device_type(&self) -> &DeviceType {
        &self.device_type
    }
    pub fn label_text(&self) -> &str {
        &self.label
    }

    pub(crate) fn to_proto(&self, provider_name: &str) -> proto::Device {
        proto::Device {
            device_id: self.id.clone(),
            provider_name: provider_name.to_owned(),
            type_id: self.device_type.id.clone(),
            type_version: self.device_type.version.clone(),
            label: self.label.clone(),
            address: self.address.clone(),
            tags: self.tags.clone(),
        }
    }
}

/// The quality of a signal value (ADPP `SignalValue.Quality`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Ok,
    Stale,
    Fault,
    Unknown,
}

impl Quality {
    pub(crate) fn to_proto(self) -> proto::signal_value::Quality {
        use proto::signal_value::Quality as Q;
        match self {
            Self::Ok => Q::Ok,
            Self::Stale => Q::Stale,
            Self::Fault => Q::Fault,
            Self::Unknown => Q::Unknown,
        }
    }
}

/// A signal value with its quality and observation time.
///
/// A plain value converts into a reading of quality OK with no time of its
/// own; the SDK then stamps the time of the read. A reading older than its
/// signal's `stale_after_ms` is reported STALE.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub value: Value,
    pub quality: Quality,
    pub observed_at: Option<SystemTime>,
}

impl Reading {
    pub fn new(value: impl Into<Value>) -> Self {
        Self { value: value.into(), quality: Quality::Ok, observed_at: None }
    }

    pub fn quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self
    }

    pub fn observed_at(mut self, at: SystemTime) -> Self {
        self.observed_at = Some(at);
        self
    }
}

impl<T: Into<Value>> From<T> for Reading {
    fn from(value: T) -> Self {
        Reading::new(value)
    }
}

/// What `read` returns: a reading for every signal id asked for.
pub type Readings = HashMap<String, Reading>;

/// What `call` returns: a value for each declared result, and nothing else.
pub type Results = HashMap<String, Value>;

/// An ADPP status other than OK, for [`ProviderError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusCode {
    InvalidArgument,
    NotFound,
    FailedPrecondition,
    OutOfRange,
    Unimplemented,
    DeadlineExceeded,
    Unavailable,
    ResourceExhausted,
    Internal,
    DataLoss,
}

impl StatusCode {
    pub(crate) fn to_proto(self) -> proto::status::Code {
        use proto::status::Code as C;
        match self {
            Self::InvalidArgument => C::InvalidArgument,
            Self::NotFound => C::NotFound,
            Self::FailedPrecondition => C::FailedPrecondition,
            Self::OutOfRange => C::OutOfRange,
            Self::Unimplemented => C::Unimplemented,
            Self::DeadlineExceeded => C::DeadlineExceeded,
            Self::Unavailable => C::Unavailable,
            Self::ResourceExhausted => C::ResourceExhausted,
            Self::Internal => C::Internal,
            Self::DataLoss => C::DataLoss,
        }
    }
}

/// Return from `read` or `call` to answer with a specific ADPP status.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderError {
    pub code: StatusCode,
    pub message: String,
    pub details: HashMap<String, String>,
}

impl ProviderError {
    pub fn new(code: StatusCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), details: HashMap::new() }
    }

    /// Add a `Status.details` entry.
    pub fn detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// The device or its bus could not be reached.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::Unavailable, message)
    }

    /// The device is not in a state to do this.
    pub fn failed_precondition(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FailedPrecondition, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::Internal, message)
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderError {}

/// The provider's overall state (ADPP `ProviderHealth.State`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    Ok,
    Degraded,
    Fault,
}

/// What `health` reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub state: HealthState,
    pub message: String,
    pub metrics: HashMap<String, String>,
}

impl Health {
    pub fn ok() -> Self {
        Self { state: HealthState::Ok, message: String::new(), metrics: HashMap::new() }
    }

    pub fn new(state: HealthState, message: impl Into<String>) -> Self {
        Self { state, message: message.into(), metrics: HashMap::new() }
    }

    pub fn metric(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metrics.insert(key.into(), value.into());
        self
    }
}

/// A device's state (ADPP `DeviceHealth.State`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState {
    Ok,
    Stale,
    Unreachable,
    Fault,
}

/// What `device_health` reports for one device.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceHealth {
    pub state: DeviceState,
    pub message: String,
    pub metrics: HashMap<String, String>,
}

impl DeviceHealth {
    pub fn new(state: DeviceState, message: impl Into<String>) -> Self {
        Self { state, message: message.into(), metrics: HashMap::new() }
    }

    pub fn metric(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metrics.insert(key.into(), value.into());
        self
    }
}

/// Whether a host requirement holds (executable profile §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostStatus {
    Met,
    Unmet,
    /// The provider cannot tell on this host. It does not fail the check.
    Unknown,
}

impl HostStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Unmet => "unmet",
            Self::Unknown => "unknown",
        }
    }
}

/// One entry of the `--check-host` envelope (executable profile §6).
#[derive(Clone, Debug, PartialEq)]
pub struct HostRequirement {
    pub id: String,
    pub status: HostStatus,
    pub detail: String,
    pub remedy: String,
}

impl HostRequirement {
    pub fn new(id: impl Into<String>, status: HostStatus) -> Self {
        Self { id: id.into(), status, detail: String::new(), remedy: String::new() }
    }

    pub fn met(id: impl Into<String>) -> Self {
        Self::new(id, HostStatus::Met)
    }

    pub fn unmet(id: impl Into<String>) -> Self {
        Self::new(id, HostStatus::Unmet)
    }

    pub fn unknown(id: impl Into<String>) -> Self {
        Self::new(id, HostStatus::Unknown)
    }

    /// What was checked and found, in words an operator can act on.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// For an unmet requirement: what to change on the host.
    pub fn remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = remedy.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_ids_count_from_one() {
        let t = DeviceType::new("t").function(Function::new("a")).function(Function::new("b")).build();
        assert_eq!(t.function_by_id(1).unwrap().name(), "a");
        assert_eq!(t.function_by_id(2).unwrap().id(), 2);
        assert!(t.function_by_id(0).is_none());
        assert!(t.function_by_id(3).is_none());
    }

    #[test]
    #[should_panic(expected = "must be snake_case")]
    fn signal_id_must_be_snake_case() {
        let _ = Signal::new("ph.value", ValueType::Double);
    }

    #[test]
    #[should_panic(expected = "no default signal")]
    fn a_type_with_signals_needs_a_default() {
        let _ = DeviceType::new("t").signal(Signal::new("x", ValueType::Bool).not_default()).build();
    }

    #[test]
    #[should_panic(expected = "duplicate function")]
    fn function_names_are_unique() {
        let _ = DeviceType::new("t").function(Function::new("a")).function(Function::new("a"));
    }

    #[test]
    #[should_panic(expected = "only to numeric types")]
    fn bounds_need_a_numeric_type() {
        let _ = Arg::string("s").min(1);
    }

    #[test]
    #[should_panic(expected = "does not fit UINT64")]
    fn a_bound_must_fit_its_type() {
        let _ = Arg::uint64("u").min(-1);
    }

    #[test]
    #[should_panic(expected = "is above max")]
    fn min_must_not_exceed_max() {
        let _ = Arg::int64("i").range(5, 4);
    }

    #[test]
    #[should_panic(expected = "must be finite")]
    fn bounds_must_be_finite() {
        let _ = Arg::double("d").max(f64::INFINITY);
    }

    #[test]
    #[should_panic(expected = "only to STRING")]
    fn allowed_values_need_a_string() {
        let _ = Arg::int64("i").allowed_values(["a"]);
    }

    #[test]
    fn integer_bounds_convert_to_double() {
        let spec = Arg::double("d").range(0, 100).to_proto();
        assert_eq!((spec.min_double, spec.max_double), (Some(0.0), Some(100.0)));
    }

    #[test]
    fn coerce_converts_integers_that_fit() {
        assert_eq!(Value::Int64(3).coerce(ValueType::Uint64), Ok(Value::Uint64(3)));
        assert!(Value::Int64(-1).coerce(ValueType::Uint64).is_err());
        assert!(Value::Uint64(u64::MAX).coerce(ValueType::Int64).is_err());
        assert_eq!(Value::Int64(2).coerce(ValueType::Double), Ok(Value::Double(2.0)));
        assert!(Value::Double(2.0).coerce(ValueType::Int64).is_err());
        assert!(Value::Bool(true).coerce(ValueType::Int64).is_err());
    }
}
