//! Windows 实现：动态加载 rime.dll（`LoadLibraryW`/`GetProcAddress`）。
//!
//! 踩坑（见 spikes/librime-sys/README.md）：
//! - librime 1.17 的 `RimeSessionId` 是 `uintptr_t`（64 位），必须用 `usize`；
//! - GNU 工具链下 `raw-dylib` 运行时导入不可靠，统一用显式动态加载；
//! - Weasel 安装包的 rime.dll 是 x86，须取 librime 官方 `Windows-msvc-x64` 包；
//! - `RimeTraits` 的路径指针 librime 会保存引用，CString 须随引擎存活。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

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

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

type FnSetupLogging = unsafe extern "C" fn();
type FnInitialize = unsafe extern "C" fn(traits: *mut RimeTraits);
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

/// 动态加载得到的函数指针集合（模块句柄保持存活）。
struct Rime {
    _module: *mut c_void,
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
    fn load(dll_path: &Path) -> Result<Self, RimeError> {
        unsafe {
            let wide: Vec<u16> = dll_path
                .to_str()
                .ok_or_else(|| RimeError::Load("路径非 UTF-8".into()))?
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let module = LoadLibraryW(wide.as_ptr());
            if module.is_null() {
                return Err(RimeError::Load(format!(
                    "LoadLibraryW 失败: {}",
                    dll_path.display()
                )));
            }
            let get = |name: &str| -> Result<*mut c_void, RimeError> {
                let cname = CString::new(name).map_err(|e| RimeError::Load(e.to_string()))?;
                let p = GetProcAddress(module, cname.as_ptr() as *const u8);
                if p.is_null() {
                    Err(RimeError::Load(format!("GetProcAddress 失败: {name}")))
                } else {
                    Ok(p)
                }
            };
            let rime = Rime {
                _module: module,
                setup_logging: std::mem::transmute::<*mut c_void, FnSetupLogging>(get(
                    "RimeSetupLogging",
                )?),
                initialize: std::mem::transmute::<*mut c_void, FnInitialize>(get(
                    "RimeInitialize",
                )?),
                finalize: std::mem::transmute::<*mut c_void, FnFinalize>(get("RimeFinalize")?),
                create_session: std::mem::transmute::<*mut c_void, FnCreateSession>(get(
                    "RimeCreateSession",
                )?),
                destroy_session: std::mem::transmute::<*mut c_void, FnDestroySession>(get(
                    "RimeDestroySession",
                )?),
                start_maintenance: std::mem::transmute::<*mut c_void, FnStartMaintenance>(get(
                    "RimeStartMaintenance",
                )?),
                join_maintenance_thread: std::mem::transmute::<*mut c_void, FnJoinMaintenanceThread>(
                    get("RimeJoinMaintenanceThread")?,
                ),
                simulate_key_sequence: std::mem::transmute::<*mut c_void, FnSimulateKeySequence>(
                    get("RimeSimulateKeySequence")?,
                ),
                get_context: std::mem::transmute::<*mut c_void, FnGetContext>(get(
                    "RimeGetContext",
                )?),
                free_context: std::mem::transmute::<*mut c_void, FnFreeContext>(get(
                    "RimeFreeContext",
                )?),
                get_schema_list: std::mem::transmute::<*mut c_void, FnGetSchemaList>(get(
                    "RimeGetSchemaList",
                )?),
                free_schema_list: std::mem::transmute::<*mut c_void, FnFreeSchemaList>(get(
                    "RimeFreeSchemaList",
                )?),
                select_schema: std::mem::transmute::<*mut c_void, FnSelectSchema>(get(
                    "RimeSelectSchema",
                )?),
            };
            Ok(rime)
        }
    }
}

impl Drop for Rime {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self._module);
        }
    }
}

/// Rime 引擎：持有模块句柄 + 初始化状态 + 目录字符串（librime 保存指针引用）。
pub struct RimeEngine {
    inner: Rime,
    _shared: CString,
    _user: CString,
    _app: CString,
    _log_dir: CString,
    shared_dir: PathBuf,
}

// SAFETY: rime.dll 内部对全局 Service 与会话有锁；本引擎所有可变操作由调用方
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
    /// 加载 rime.dll 并初始化 + 首次部署（同步等待完成）。
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

    /// 需真实 rime.dll 与数据：设 VERBA_RIME_DLL/SHARED/USER 后运行；
    /// 未设环境变量时跳过（保持 CI 可过）。
    #[test]
    fn rime_engine_translates_pinyin_and_wubi() {
        let Ok(dll) = std::env::var("VERBA_RIME_DLL") else {
            eprintln!("跳过：未设置 VERBA_RIME_DLL");
            return;
        };
        let shared = std::env::var("VERBA_RIME_SHARED").unwrap_or_default();
        let user = std::env::var("VERBA_RIME_USER").unwrap_or_default();
        let cfg = RimeConfig::load(Path::new(&dll), Path::new(&shared), Path::new(&user));
        let engine = RimeEngine::new(&cfg).expect("加载 rime");
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
