//! 跨平台 Rime（librime）实现：用 `libloading` 加载，并走 **现代 `rime_get_api()`** 返回的
//! `RimeApi` 结构体（librime 1.x 的正统入口；兼容 Windows rime.dll / macOS librime.dylib /
//! Squirrel 内置等，避免逐个 dlsym 单个符号在不同构建下找不到）。
//!
//! 踩坑（见 spikes/librime-sys/README.md）：
//! - librime 1.17 的 `RimeSessionId` 是 `uintptr_t`（64 位），必须用 `usize`；
//! - 单个符号（RimeInitialize 等）在部分构建（如 Homebrew macOS bottle）以 C++ 修饰名导出，
//!   不可靠；统一走 `rime_get_api()` 结构体。
//! - macOS 需 `librime.dylib`（librime 原生支持 macOS，如 Squirrel 即基于 librime）；
//! - `RimeTraits` 的路径指针 librime 会保存引用，CString 须随引擎存活。

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use libloading::Library;

use crate::{RimeCandidate, RimeConfig, RimeError, RimeSchema};

type RimeBool = i32;
type RimeSessionId = usize; // librime 1.17+：uintptr_t

type RimeNotificationHandler = unsafe extern "C" fn(
    context_object: *mut c_void,
    session_id: RimeSessionId,
    message_type: *const c_char,
    message_value: *const c_char,
);

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

#[repr(C)]
struct RimeCommit {
    data_size: i32,
    text: *mut c_char,
}

#[repr(C)]
struct RimeStatus {
    data_size: i32,
    schema_id: *mut c_char,
    schema_name: *mut c_char,
    is_disabled: RimeBool,
    is_composing: RimeBool,
    is_ascii_mode: RimeBool,
    is_full_shape: RimeBool,
    is_simplified: RimeBool,
    is_traditional: RimeBool,
    is_ascii_punct: RimeBool,
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
struct RimeConfigC {
    ptr: *mut c_void,
}

#[repr(C)]
struct RimeConfigIterator {
    list: *mut c_void,
    map: *mut c_void,
    index: i32,
    key: *const c_char,
    path: *const c_char,
}

#[repr(C)]
struct RimeCustomApi {
    data_size: i32,
}

#[repr(C)]
struct RimeModule {
    data_size: i32,
    module_name: *const c_char,
    initialize: unsafe extern "C" fn(),
    finalize: unsafe extern "C" fn(),
    get_api: unsafe extern "C" fn() -> *mut RimeCustomApi,
}

#[repr(C)]
struct RimeCandidateListIterator {
    ptr: *mut c_void,
    index: i32,
    candidate: RimeCandidateC,
}

#[repr(C)]
struct RimeCandidatePreview {
    data_size: i32,
    text_before_selection: *mut c_char,
    selected_text: *mut c_char,
    text_after_selection: *mut c_char,
}

#[repr(C)]
struct RimeStringSlice {
    str: *const c_char,
    length: usize,
}

/// librime 1.x 的 `RimeApi`（由 `rime_get_api()` 返回）。字段顺序严格对照 `rime_api.h`。
#[repr(C)]
struct RimeApi {
    data_size: i32,
    setup: unsafe extern "C" fn(traits: *mut RimeTraits),
    set_notification_handler:
        unsafe extern "C" fn(handler: RimeNotificationHandler, context_object: *mut c_void),
    initialize: unsafe extern "C" fn(traits: *mut RimeTraits),
    finalize: unsafe extern "C" fn(),
    start_maintenance: unsafe extern "C" fn(full_check: RimeBool) -> RimeBool,
    is_maintenance_mode: unsafe extern "C" fn() -> RimeBool,
    join_maintenance_thread: unsafe extern "C" fn(),
    deployer_initialize: unsafe extern "C" fn(traits: *mut RimeTraits),
    prebuild: unsafe extern "C" fn() -> RimeBool,
    deploy: unsafe extern "C" fn() -> RimeBool,
    deploy_schema: unsafe extern "C" fn(schema_file: *const c_char) -> RimeBool,
    deploy_config_file:
        unsafe extern "C" fn(file_name: *const c_char, version_key: *const c_char) -> RimeBool,
    sync_user_data: unsafe extern "C" fn() -> RimeBool,
    create_session: unsafe extern "C" fn() -> RimeSessionId,
    find_session: unsafe extern "C" fn(session_id: RimeSessionId) -> RimeBool,
    destroy_session: unsafe extern "C" fn(session_id: RimeSessionId) -> RimeBool,
    cleanup_stale_sessions: unsafe extern "C" fn(),
    cleanup_all_sessions: unsafe extern "C" fn(),
    process_key:
        unsafe extern "C" fn(session_id: RimeSessionId, keycode: i32, mask: i32) -> RimeBool,
    commit_composition: unsafe extern "C" fn(session_id: RimeSessionId) -> RimeBool,
    clear_composition: unsafe extern "C" fn(session_id: RimeSessionId),
    get_commit:
        unsafe extern "C" fn(session_id: RimeSessionId, commit: *mut RimeCommit) -> RimeBool,
    free_commit: unsafe extern "C" fn(commit: *mut RimeCommit) -> RimeBool,
    get_context:
        unsafe extern "C" fn(session_id: RimeSessionId, context: *mut RimeContext) -> RimeBool,
    free_context: unsafe extern "C" fn(ctx: *mut RimeContext) -> RimeBool,
    get_status:
        unsafe extern "C" fn(session_id: RimeSessionId, status: *mut RimeStatus) -> RimeBool,
    free_status: unsafe extern "C" fn(status: *mut RimeStatus) -> RimeBool,
    set_option:
        unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char, value: RimeBool),
    get_option: unsafe extern "C" fn(session_id: RimeSessionId, option: *const c_char) -> RimeBool,
    set_property:
        unsafe extern "C" fn(session_id: RimeSessionId, prop: *const c_char, value: *const c_char),
    get_property: unsafe extern "C" fn(
        session_id: RimeSessionId,
        prop: *const c_char,
        value: *mut c_char,
        buffer_size: usize,
    ) -> RimeBool,
    get_schema_list: unsafe extern "C" fn(schema_list: *mut RimeSchemaList) -> RimeBool,
    free_schema_list: unsafe extern "C" fn(schema_list: *mut RimeSchemaList),
    get_current_schema: unsafe extern "C" fn(
        session_id: RimeSessionId,
        schema_id: *mut c_char,
        buffer_size: usize,
    ) -> RimeBool,
    select_schema:
        unsafe extern "C" fn(session_id: RimeSessionId, schema_id: *const c_char) -> RimeBool,
    schema_open:
        unsafe extern "C" fn(schema_id: *const c_char, config: *mut RimeConfigC) -> RimeBool,
    config_open:
        unsafe extern "C" fn(config_id: *const c_char, config: *mut RimeConfigC) -> RimeBool,
    config_close: unsafe extern "C" fn(config: *mut RimeConfigC) -> RimeBool,
    config_get_bool: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut RimeBool,
    ) -> RimeBool,
    config_get_int: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut i32,
    ) -> RimeBool,
    config_get_double: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut f64,
    ) -> RimeBool,
    config_get_string: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut c_char,
        buffer_size: usize,
    ) -> RimeBool,
    config_get_cstring:
        unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char) -> *const c_char,
    config_update_signature:
        unsafe extern "C" fn(config: *mut RimeConfigC, signer: *const c_char) -> RimeBool,
    config_begin_map: unsafe extern "C" fn(
        iterator: *mut RimeConfigIterator,
        config: *mut RimeConfigC,
        key: *const c_char,
    ) -> RimeBool,
    config_next: unsafe extern "C" fn(iterator: *mut RimeConfigIterator) -> RimeBool,
    config_end: unsafe extern "C" fn(iterator: *mut RimeConfigIterator),
    simulate_key_sequence:
        unsafe extern "C" fn(session_id: RimeSessionId, key_sequence: *const c_char) -> RimeBool,
    register_module: unsafe extern "C" fn(module: *mut RimeModule) -> RimeBool,
    find_module: unsafe extern "C" fn(module_name: *const c_char) -> *mut RimeModule,
    run_task: unsafe extern "C" fn(task_name: *const c_char) -> RimeBool,
    get_shared_data_dir: unsafe extern "C" fn() -> *const c_char,
    get_user_data_dir: unsafe extern "C" fn() -> *const c_char,
    get_sync_dir: unsafe extern "C" fn() -> *const c_char,
    get_user_id: unsafe extern "C" fn() -> *const c_char,
    get_user_data_sync_dir: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    config_init: unsafe extern "C" fn(config: *mut RimeConfigC) -> RimeBool,
    config_load_string:
        unsafe extern "C" fn(config: *mut RimeConfigC, yaml: *const c_char) -> RimeBool,
    config_set_bool: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: RimeBool,
    ) -> RimeBool,
    config_set_int:
        unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char, value: i32) -> RimeBool,
    config_set_double:
        unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char, value: f64) -> RimeBool,
    config_set_string: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *const c_char,
    ) -> RimeBool,
    config_get_item: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut RimeConfigC,
    ) -> RimeBool,
    config_set_item: unsafe extern "C" fn(
        config: *mut RimeConfigC,
        key: *const c_char,
        value: *mut RimeConfigC,
    ) -> RimeBool,
    config_clear: unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char) -> RimeBool,
    config_create_list:
        unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char) -> RimeBool,
    config_create_map:
        unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char) -> RimeBool,
    config_list_size: unsafe extern "C" fn(config: *mut RimeConfigC, key: *const c_char) -> usize,
    config_begin_list: unsafe extern "C" fn(
        iterator: *mut RimeConfigIterator,
        config: *mut RimeConfigC,
        key: *const c_char,
    ) -> RimeBool,
    get_input: unsafe extern "C" fn(session_id: RimeSessionId) -> *const c_char,
    get_caret_pos: unsafe extern "C" fn(session_id: RimeSessionId) -> usize,
    select_candidate: unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    get_version: unsafe extern "C" fn() -> *const c_char,
    set_caret_pos: unsafe extern "C" fn(session_id: RimeSessionId, caret_pos: usize),
    select_candidate_on_current_page:
        unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    candidate_list_begin: unsafe extern "C" fn(
        session_id: RimeSessionId,
        iterator: *mut RimeCandidateListIterator,
    ) -> RimeBool,
    candidate_list_next: unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator) -> RimeBool,
    candidate_list_end: unsafe extern "C" fn(iterator: *mut RimeCandidateListIterator),
    user_config_open:
        unsafe extern "C" fn(config_id: *const c_char, config: *mut RimeConfigC) -> RimeBool,
    candidate_list_from_index: unsafe extern "C" fn(
        session_id: RimeSessionId,
        iterator: *mut RimeCandidateListIterator,
        index: i32,
    ) -> RimeBool,
    get_prebuilt_data_dir: unsafe extern "C" fn() -> *const c_char,
    get_staging_dir: unsafe extern "C" fn() -> *const c_char,
    commit_proto: unsafe extern "C" fn(session_id: RimeSessionId, commit_builder: *mut c_void),
    context_proto: unsafe extern "C" fn(session_id: RimeSessionId, context_builder: *mut c_void),
    status_proto: unsafe extern "C" fn(session_id: RimeSessionId, status_builder: *mut c_void),
    get_state_label: unsafe extern "C" fn(
        session_id: RimeSessionId,
        option_name: *const c_char,
        state: RimeBool,
    ) -> *const c_char,
    delete_candidate: unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    delete_candidate_on_current_page:
        unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    get_state_label_abbreviated: unsafe extern "C" fn(
        session_id: RimeSessionId,
        option_name: *const c_char,
        state: RimeBool,
        abbreviated: RimeBool,
    ) -> RimeStringSlice,
    set_input: unsafe extern "C" fn(session_id: RimeSessionId, input: *const c_char) -> RimeBool,
    get_shared_data_dir_s: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    get_user_data_dir_s: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    get_prebuilt_data_dir_s: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    get_staging_dir_s: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    get_sync_dir_s: unsafe extern "C" fn(dir: *mut c_char, buffer_size: usize),
    highlight_candidate: unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    highlight_candidate_on_current_page:
        unsafe extern "C" fn(session_id: RimeSessionId, index: usize) -> RimeBool,
    change_page: unsafe extern "C" fn(session_id: RimeSessionId, backward: RimeBool) -> RimeBool,
    get_candidate_preview: unsafe extern "C" fn(
        session_id: RimeSessionId,
        preview: *mut RimeCandidatePreview,
    ) -> RimeBool,
    free_candidate_preview: unsafe extern "C" fn(preview: *mut RimeCandidatePreview) -> RimeBool,
}

/// 动态加载得到的 `RimeApi` 指针。
///
/// `Library` 句柄在加载时泄露（进程生命周期内不 dlclose）：librime 为进程级单例，
/// 卸载（dlclose）时其全局状态仍在，会在退出时 SIGSEGV（Squirrel/Weasel 亦从不卸载）。
struct Rime {
    api: *const RimeApi,
}

impl Rime {
    fn load(lib_path: &Path) -> Result<Self, RimeError> {
        unsafe {
            let lib = Library::new(lib_path).map_err(|e| {
                RimeError::Load(format!("动态加载失败: {} ({e})", lib_path.display()))
            })?;
            let get_api: libloading::Symbol<unsafe extern "C" fn() -> *const RimeApi> = lib
                .get(b"rime_get_api\0")
                .map_err(|e| RimeError::Load(format!("取 rime_get_api 失败: {e}")))?;
            let api = get_api();
            if api.is_null() {
                return Err(RimeError::Load("rime_get_api 返回空指针".into()));
            }
            // 进程生命周期内不 dlclose（见 struct 注释）。
            std::mem::forget(lib);
            Ok(Rime { api })
        }
    }
}

/// Rime 引擎：持有库句柄 + `RimeApi` + 初始化状态 + 目录字符串（librime 保存指针引用）。
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

        let api = inner.api;
        unsafe {
            ((*api).setup)(&mut traits);
            ((*api).initialize)(&mut traits);
            // 首次运行需部署（编译 schema/词典）；同步等待完成。
            ((*api).start_maintenance)(0);
            ((*api).join_maintenance_thread)();
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
        let api = self.inner.api;
        unsafe {
            let mut list = RimeSchemaList {
                size: 0,
                list: std::ptr::null_mut(),
            };
            if ((*api).get_schema_list)(&mut list) == 0 {
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
            ((*api).free_schema_list)(&mut list);
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
        let api = self.inner.api;
        unsafe {
            let session = ((*api).create_session)();
            if session == 0 {
                return Err(RimeError::Input("创建会话失败".into()));
            }
            let result = (|| {
                let sel = CString::new(schema).map_err(|e| RimeError::Input(e.to_string()))?;
                if ((*api).select_schema)(session, sel.as_ptr()) == 0 {
                    return Err(RimeError::Input(format!("选择方案失败: {schema}")));
                }
                let seq = CString::new(input).map_err(|e| RimeError::Input(e.to_string()))?;
                if ((*api).simulate_key_sequence)(session, seq.as_ptr()) == 0 {
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
                if ((*api).get_context)(session, &mut ctx) == 0 {
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
                ((*api).free_context)(&mut ctx);
                Ok(out)
            })();
            ((*api).destroy_session)(session);
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
        // 只做优雅清理（finalize）；库句柄已泄露（不 dlclose）。
        unsafe {
            let api = self.inner.api;
            ((*api).finalize)();
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
