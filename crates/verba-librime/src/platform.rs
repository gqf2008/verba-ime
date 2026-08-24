//! 跨平台 Rime（librime）实现：用 `libloading` 动态加载
//! （Windows `rime.dll` / macOS `librime.dylib` / 其它 Unix `librime.so`）。
//!
//! 踩坑（见 spikes/librime-sys/README.md）：
//! - librime 1.17 的 `RimeSessionId` 是 `uintptr_t`（64 位），必须用 `usize`；
//! - GNU 工具链下 `raw-dylib` 运行时导入不可靠，统一用显式动态加载；
//! - Weasel 安装包的 rime.dll 是 x86，须取 librime 官方 `Windows-msvc-x64` 包；
//! - macOS 需 `librime.dylib`（librime 原生支持 macOS，如 Squirrel 即基于 librime）；
//! - `RimeTraits` 的路径指针 librime 会保存引用，CString 须随引擎存活。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use libloading::Library;

use crate::{RimeCandidate, RimeConfig, RimeError, RimeSchema};

type RimeBool = i32;
type RimeSessionId = usize; // librime 1.17+：uintptr_t

#[repr(C)]
struct RimeTraits {
    data_size: i32,
    shared_data_dir: *const c_char,
    user_data_dir: *const c_char,
    distribution_name: *const c_char,
    distribution_code_name: *const c_char,
    distribution_version: *const c_char,
    app_name: *const c_char,
    modules: *const *const c_char,
    min_log_level: i32,
    log_dir: *const c_char,
    prebuilt_data_dir: *const c_char,
    staging_dir: *const c_char,
}

#[repr(C)]
struct RimeSchemaListItem {
    schema_id: *mut c_char,
    name: *mut c_char,
    reserved: *mut c_void,
}

#[repr(C)]
struct RimeSchemaList {
    size: usize,
    list: *mut RimeSchemaListItem,
}

#[repr(C)]
struct RimeComposition {
    length: i32,
    cursor_pos: i32,
    sel_start: i32,
    sel_end: i32,
    preedit: *mut c_char,
}

#[repr(C)]
struct RimeCandidateC {
    text: *mut c_char,
    comment: *mut c_char,
    reserved: *mut c_void,
}

#[repr(C)]
struct RimeMenu {
    page_size: i32,
    page_no: i32,
    is_last_page: RimeBool,
    highlighted_candidate_index: i32,
    num_candidates: i32,
    candidates: *mut RimeCandidateC,
    select_keys: *mut c_char,
}

#[repr(C)]
struct RimeContext {
    data_size: i32,
    composition: RimeComposition,
    menu: RimeMenu,
    commit_text_preview: *mut c_char,
    select_labels: *mut *mut c_char,
}

type FnSetupLogging = unsafe extern "C" fn();
type FnInitialize = unsafe extern "C" fn(traits: *mut RimeTraits) -> RimeBool;
type FnFinalize = unsafe extern "C" fn();
type FnCreateSession = unsafe extern "C" fn() -> RimeSessionId;
type FnDestroySession = unsafe extern "C" fn(session: RimeSessionId) -> RimeBool;
type FnStartMaintenance = unsafe extern "C" fn(full_check: RimeBool) -> RimeBool;
type FnJoinMaintenanceThread = unsafe extern "C" fn();
type FnSimulateKeySequence =
    unsafe extern "C" fn(session: RimeSessionId, key_sequence: *const c_char) -> RimeBool;
type FnGetContext =
    unsafe extern "C" fn(session: RimeSessionId, context: *mut RimeContext) -> RimeBool;
type FnFreeContext = unsafe extern "C" fn(context: *mut RimeContext) -> RimeBool;
type FnGetSchemaList = unsafe extern "C" fn(schema_list: *mut RimeSchemaList) -> RimeBool;
type FnFreeSchemaList = unsafe extern "C" fn(schema_list: *mut RimeSchemaList);
type FnSelectSchema =
    unsafe extern "C" fn(session: RimeSessionId, schema_id: *const c_char) -> RimeBool;

/// 动态加载得到的函数指针集合（`Library` 句柄保持存活）。
struct Rime {
    _lib: Library,
    setup_logging: FnSetupLogging,
    initialize: FnInitialize,
    finalize: FnFinalize,
    create_session: FnCreateSession,
    destroy_session: FnDestroySession,
    start_maintenance: FnStartMaintenance,
    join_maintenance_thread: FnJoinMaintenanceThread,
    simulate_key_sequence: FnSimulateKeySequence,
    get_context: FnGetContext,
    free_context: FnFreeContext,
    get_schema_list: FnGetSchemaList,
    free_schema_list: FnFreeSchemaList,
    select_schema: FnSelectSchema,
}

impl Rime {
    fn load(lib_path: &Path) -> Result<Self, RimeError> {
        unsafe {
            let lib = Library::new(lib_path).map_err(|e| {
                RimeError::Load(format!("动态加载失败: {} ({e})", lib_path.display()))
            })?;
            let get = |name: &[u8]| -> Result<*mut c_void, RimeError> {
                // Symbol<T> deref 到 T（函数指针），*sym 即原始函数指针。
                lib.get::<unsafe extern "C" fn()>(name)
                    .map(|sym| *sym as *mut c_void)
                    .map_err(|e| RimeError::Load(format!("取符号失败 {name:?}: {e}")))
            };
            let rime = Rime {
                setup_logging: std::mem::transmute::<*mut c_void, FnSetupLogging>(get(
                    b"RimeSetupLogging\0",
                )?),
                initialize: std::mem::transmute::<*mut c_void, FnInitialize>(get(
                    b"RimeInitialize\0",
                )?),
                finalize: std::mem::transmute::<*mut c_void, FnFinalize>(get(b"RimeFinalize\0")?),
                create_session: std::mem::transmute::<*mut c_void, FnCreateSession>(get(
                    b"RimeCreateSession\0",
                )?),
                destroy_session: std::mem::transmute::<*mut c_void, FnDestroySession>(get(
                    b"RimeDestroySession\0",
                )?),
                start_maintenance: std::mem::transmute::<*mut c_void, FnStartMaintenance>(get(
                    b"RimeStartMaintenance\0",
                )?),
                join_maintenance_thread: std::mem::transmute::<*mut c_void, FnJoinMaintenanceThread>(
                    get(b"RimeJoinMaintenanceThread\0")?,
                ),
                simulate_key_sequence: std::mem::transmute::<*mut c_void, FnSimulateKeySequence>(
                    get(b"RimeSimulateKeySequence\0")?,
                ),
                get_context: std::mem::transmute::<*mut c_void, FnGetContext>(get(
                    b"RimeGetContext\0",
                )?),
                free_context: std::mem::transmute::<*mut c_void, FnFreeContext>(get(
                    b"RimeFreeContext\0",
                )?),
                get_schema_list: std::mem::transmute::<*mut c_void, FnGetSchemaList>(get(
                    b"RimeGetSchemaList\0",
                )?),
                free_schema_list: std::mem::transmute::<*mut c_void, FnFreeSchemaList>(get(
                    b"RimeFreeSchemaList\0",
                )?),
                select_schema: std::mem::transmute::<*mut c_void, FnSelectSchema>(get(
                    b"RimeSelectSchema\0",
                )?),
                _lib: lib,
            };
            Ok(rime)
        }
    }
}

/// Rime 引擎：持有库句柄 + 初始化状态 + 目录字符串（librime 保存指针引用）。
pub struct RimeEngine {
    inner: Rime,
    _shared: CString,
    _user: CString,
    _app: CString,
    _log_dir: CString,
    shared_dir: PathBuf,
}

// SAFETY: librime 内部对全局 Service 与会话有锁；本引擎所有可变操作由调用方
// （daemon 的 Mutex）串行化，函数指针与句柄可跨线程安全传递。
unsafe impl Send for RimeEngine {}
unsafe impl Sync for RimeEngine {}

fn to_rust(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

impl RimeEngine {
    /// 加载 librime 并初始化 + 首次部署（同步等待完成）。
    pub fn new(cfg: &RimeConfig) -> Result<Self, RimeError> {
        let inner = Rime::load(&cfg.dll_path)?;
        std::fs::create_dir_all(&cfg.shared_data_dir)
            .map_err(|e| RimeError::Init(format!("共享目录不可用: {e}")))?;
        std::fs::create_dir_all(&cfg.user_data_dir)
            .map_err(|e| RimeError::Init(format!("用户目录不可用: {e}")))?;

        let shared_c = CString::new(cfg.shared_data_dir.to_str().unwrap_or_default())
            .map_err(|e| RimeError::Init(e.to_string()))?;
        let user_c = CString::new(cfg.user_data_dir.to_str().unwrap_or_default())
            .map_err(|e| RimeError::Init(e.to_string()))?;
        let app_c = CString::new("verba.daemon").map_err(|e| RimeError::Init(e.to_string()))?;
        let log_dir = cfg.user_data_dir.join("log");
        std::fs::create_dir_all(&log_dir).map_err(|e| RimeError::Init(e.to_string()))?;
        let log_dir_c = CString::new(log_dir.to_str().unwrap_or_default())
            .map_err(|e| RimeError::Init(e.to_string()))?;

        let mut traits = RimeTraits {
            data_size: std::mem::size_of::<RimeTraits>() as i32,
            shared_data_dir: shared_c.as_ptr(),
            user_data_dir: user_c.as_ptr(),
            distribution_name: std::ptr::null(),
            distribution_code_name: std::ptr::null(),
            distribution_version: std::ptr::null(),
            app_name: app_c.as_ptr(),
            modules: std::ptr::null(),
            min_log_level: 1,
            log_dir: log_dir_c.as_ptr(),
            prebuilt_data_dir: std::ptr::null(),
            staging_dir: std::ptr::null(),
        };

        unsafe {
            (inner.setup_logging)();
            (inner.initialize)(&mut traits);
            // 首次运行需部署（编译 schema/词典）；同步等待完成。
            (inner.start_maintenance)(0);
            (inner.join_maintenance_thread)();
        }

        Ok(Self {
            inner,
            _shared: shared_c,
            _user: user_c,
            _app: app_c,
            _log_dir: log_dir_c,
            shared_dir: cfg.shared_data_dir.clone(),
        })
    }

    /// 已部署的方案列表（诊断/调试用）。
    pub fn schemas(&self) -> Result<Vec<RimeSchema>, RimeError> {
        unsafe {
            let mut list = RimeSchemaList {
                size: 0,
                list: std::ptr::null_mut(),
            };
            if (self.inner.get_schema_list)(&mut list) == 0 {
                return Err(RimeError::Input("获取方案列表失败".into()));
            }
            let mut out = Vec::with_capacity(list.size);
            for i in 0..list.size {
                let item = &*list.list.add(i);
                out.push(RimeSchema {
                    schema_id: to_rust(item.schema_id),
                    name: to_rust(item.name),
                });
            }
            (self.inner.free_schema_list)(&mut list);
            Ok(out)
        }
    }

    /// 对输入串（拼音/五笔码）查询候选列表（最多 `max` 个）。
    pub fn candidates(
        &self,
        input: &str,
        schema: &str,
        max: usize,
    ) -> Result<Vec<RimeCandidate>, RimeError> {
        unsafe {
            let session = (self.inner.create_session)();
            if session == 0 {
                return Err(RimeError::Input("创建会话失败".into()));
            }
            let result = (|| {
                let sel = CString::new(schema).map_err(|e| RimeError::Input(e.to_string()))?;
                if (self.inner.select_schema)(session, sel.as_ptr()) == 0 {
                    return Err(RimeError::Input(format!("选择方案失败: {schema}")));
                }
                let seq = CString::new(input).map_err(|e| RimeError::Input(e.to_string()))?;
                if (self.inner.simulate_key_sequence)(session, seq.as_ptr()) == 0 {
                    return Err(RimeError::Input(format!("输入处理失败: {input}")));
                }
                let mut ctx = RimeContext {
                    data_size: std::mem::size_of::<RimeContext>() as i32,
                    composition: RimeComposition {
                        length: 0,
                        cursor_pos: 0,
                        sel_start: 0,
                        sel_end: 0,
                        preedit: std::ptr::null_mut(),
                    },
                    menu: RimeMenu {
                        page_size: 0,
                        page_no: 0,
                        is_last_page: 0,
                        highlighted_candidate_index: 0,
                        num_candidates: 0,
                        candidates: std::ptr::null_mut(),
                        select_keys: std::ptr::null_mut(),
                    },
                    commit_text_preview: std::ptr::null_mut(),
                    select_labels: std::ptr::null_mut(),
                };
                if (self.inner.get_context)(session, &mut ctx) == 0 {
                    return Err(RimeError::Input("获取上下文失败".into()));
                }
                let n = ctx.menu.num_candidates.max(0) as usize;
                let mut out = Vec::with_capacity(n.min(max));
                for i in 0..n.min(max) {
                    let c = &*ctx.menu.candidates.add(i);
                    out.push(RimeCandidate {
                        text: to_rust(c.text),
                        comment: to_rust(c.comment),
                    });
                }
                (self.inner.free_context)(&mut ctx);
                Ok(out)
            })();
            (self.inner.destroy_session)(session);
            result
        }
    }

    /// 共享数据目录（部署产物检查/日志用）。
    pub fn shared_data_dir(&self) -> &Path {
        &self.shared_dir
    }
}

impl Drop for RimeEngine {
    fn drop(&mut self) {
        unsafe {
            (self.inner.finalize)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 需真实 librime 库与数据：设 VERBA_RIME_DLL（Windows）/
    /// VERBA_RIME_DYLIB（macOS）及 SHARED/USER 后运行；未设时跳过（保持 CI 可过）。
    #[test]
    fn rime_engine_translates_pinyin_and_wubi() {
        let dll = std::env::var("VERBA_RIME_DLL")
            .or_else(|_| std::env::var("VERBA_RIME_DYLIB"))
            .ok();
        let Some(dll) = dll else {
            eprintln!("跳过：未设置 VERBA_RIME_DLL / VERBA_RIME_DYLIB");
            return;
        };
        let shared = std::env::var("VERBA_RIME_SHARED").unwrap_or_default();
        let user = std::env::var("VERBA_RIME_USER").unwrap_or_default();
        let cfg = RimeConfig::load(Path::new(&dll), Path::new(&shared), Path::new(&user));
        let engine = RimeEngine::new(&cfg).expect("加载 librime");
        let schemas = engine.schemas().expect("方案列表");
        assert!(
            schemas.iter().any(|s| s.schema_id == "luna_pinyin"),
            "应有 luna_pinyin，实际 {schemas:?}"
        );
        let cands = engine
            .candidates("nishishui", "luna_pinyin", 5)
            .expect("拼音候选");
        assert!(
            cands.iter().any(|c| c.text.contains("你是")),
            "nishishui 应有「你是…」，实际 {cands:?}"
        );
        let wubi = engine.candidates("wqvb", "wubi86", 5).expect("五笔候选");
        assert!(
            wubi.iter().any(|c| c.text == "你好"),
            "wqvb 应含「你好」，实际 {wubi:?}"
        );
    }
}
