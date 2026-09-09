//! Store-free decode of plain link-seam values from a wRPC byte stream.
//!
//! [`wrpc_wasmtime::read_value`] needs `StoreContextMut` only in its
//! resource/stream arms — values the dispatch path already rejects at the link
//! seam. The concurrent polyfill ([`super::polyfill`]) can therefore decode results
//! without holding the store across an await, which `Accessor::with` forbids.
//! Each arm mirrors `read_value` so the wire codec stays byte-identical.

use std::io::{Error, ErrorKind, Result};

use tokio::io::{AsyncRead, AsyncReadExt as _};
use wasm_tokio::cm::AsyncReadValue as _;
use wasm_tokio::{AsyncReadCore as _, AsyncReadLeb128 as _, AsyncReadUtf8 as _};
use wasmtime::component::types::{Case, Field};
use wasmtime::component::{Type, Val};

/// Decode one plain (resource-free) value of type [`Type`] from `r` into `val`.
// One arm per `Type` variant, mirroring `wrpc_wasmtime::read_value`.
#[allow(clippy::too_many_lines)]
pub(super) async fn read_plain_value<R>(r: &mut R, val: &mut Val, ty: &Type) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    match ty {
        Type::Bool => {
            let v = r.read_bool().await?;
            *val = Val::Bool(v);
            Ok(())
        }
        Type::S8 => {
            let v = r.read_i8().await?;
            *val = Val::S8(v);
            Ok(())
        }
        Type::U8 => {
            let v = r.read_u8().await?;
            *val = Val::U8(v);
            Ok(())
        }
        Type::S16 => {
            let v = r.read_i16_leb128().await?;
            *val = Val::S16(v);
            Ok(())
        }
        Type::U16 => {
            let v = r.read_u16_leb128().await?;
            *val = Val::U16(v);
            Ok(())
        }
        Type::S32 => {
            let v = r.read_i32_leb128().await?;
            *val = Val::S32(v);
            Ok(())
        }
        Type::U32 => {
            let v = r.read_u32_leb128().await?;
            *val = Val::U32(v);
            Ok(())
        }
        Type::S64 => {
            let v = r.read_i64_leb128().await?;
            *val = Val::S64(v);
            Ok(())
        }
        Type::U64 => {
            let v = r.read_u64_leb128().await?;
            *val = Val::U64(v);
            Ok(())
        }
        Type::Float32 => {
            let v = r.read_f32_le().await?;
            *val = Val::Float32(v);
            Ok(())
        }
        Type::Float64 => {
            let v = r.read_f64_le().await?;
            *val = Val::Float64(v);
            Ok(())
        }
        Type::Char => {
            let v = r.read_char_utf8().await?;
            *val = Val::Char(v);
            Ok(())
        }
        Type::String => {
            let mut s = String::default();
            r.read_core_name(&mut s).await?;
            *val = Val::String(s);
            Ok(())
        }
        Type::List(ty) => {
            let n = r.read_u32_leb128().await?;
            let n = n.try_into().unwrap_or(usize::MAX);
            // The length is wire-supplied: cap the pre-allocation and let the
            // vector grow as elements actually decode, so a corrupt frame
            // cannot force a huge up-front allocation.
            let mut vs = Vec::with_capacity(n.min(1024));
            let ty = ty.ty();
            for _ in 0..n {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                vs.push(v);
            }
            *val = Val::List(vs);
            Ok(())
        }
        Type::Record(ty) => {
            let fields = ty.fields();
            let mut vs = Vec::with_capacity(fields.len());
            for Field { name, ty } in fields {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                vs.push((name.to_string(), v));
            }
            *val = Val::Record(vs);
            Ok(())
        }
        Type::Tuple(ty) => {
            let types = ty.types();
            let mut vs = Vec::with_capacity(types.len());
            for ty in types {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                vs.push(v);
            }
            *val = Val::Tuple(vs);
            Ok(())
        }
        Type::Variant(ty) => {
            let discriminant = r.read_u32_leb128().await?;
            let discriminant =
                discriminant.try_into().map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
            let Case { name, ty } = ty.cases().nth(discriminant).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("unknown variant discriminant `{discriminant}`"),
                )
            })?;
            let name = name.to_string();
            if let Some(ty) = ty {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                *val = Val::Variant(name, Some(Box::new(v)));
            } else {
                *val = Val::Variant(name, None);
            }
            Ok(())
        }
        Type::Enum(ty) => {
            let discriminant = r.read_u32_leb128().await?;
            let discriminant =
                discriminant.try_into().map_err(|err| Error::new(ErrorKind::InvalidInput, err))?;
            let name = ty.names().nth(discriminant).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("unknown enum discriminant `{discriminant}`"),
                )
            })?;
            *val = Val::Enum(name.to_string());
            Ok(())
        }
        Type::Option(ty) => {
            let ok = r.read_option_status().await?;
            if ok {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty.ty())).await?;
                *val = Val::Option(Some(Box::new(v)));
            } else {
                *val = Val::Option(None);
            }
            Ok(())
        }
        Type::Result(ty) => {
            let ok = r.read_result_status().await?;
            if ok {
                if let Some(ty) = ty.ok() {
                    let mut v = Val::Bool(false);
                    Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                    *val = Val::Result(Ok(Some(Box::new(v))));
                } else {
                    *val = Val::Result(Ok(None));
                }
            } else if let Some(ty) = ty.err() {
                let mut v = Val::Bool(false);
                Box::pin(read_plain_value(r, &mut v, &ty)).await?;
                *val = Val::Result(Err(Some(Box::new(v))));
            } else {
                *val = Val::Result(Err(None));
            }
            Ok(())
        }
        Type::Flags(ty) => {
            let names = ty.names();
            // The wire carries `ceil(bits / 8)` little-endian bytes; wrpc's
            // encoder emits one byte even for a zero-flag type, so floor at 1.
            let mut buf = vec![0; names.len().div_ceil(8).max(1)];
            r.read_exact(&mut buf).await?;
            let set: usize = buf.iter().map(|b| b.count_ones() as usize).sum();
            let mut vs = Vec::with_capacity(set);
            for (i, name) in names.enumerate() {
                if buf[i / 8] & (1 << (i % 8)) != 0 {
                    vs.push(name.to_string());
                }
            }
            *val = Val::Flags(vs);
            Ok(())
        }
        Type::Own(_) | Type::Borrow(_) | Type::Future(_) | Type::Stream(_) | Type::ErrorContext => {
            Err(Error::new(
                ErrorKind::Unsupported,
                "a resource, stream, or future cannot cross the link seam",
            ))
        }
        Type::Map(..) => Err(Error::new(ErrorKind::Unsupported, "`map` type not supported")),
        // Mirrors `wrpc_wasmtime::read_value`, which rejects these too.
        Type::FixedLengthList(..) => {
            Err(Error::new(ErrorKind::Unsupported, "`fixed-length-list` type not supported"))
        }
    }
}
