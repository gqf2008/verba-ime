//! Windows 下加载 `vendor\rime.dll`（Weasel 0.17.4 预编译 librime），验证：
//!   1) RimeInitialize / 首次部署（StartMaintenance + JoinMaintenanceThread）
//!   2) 拼音方案（luna_pinyin）：整句输入模拟 → 上屏文本
//!   3) 五笔方案（wubi86）：加载 + 编码模拟 → 上屏文本
//! 用法：
//!   pwsh fetch-vendor.ps1        # 首次：下载 Weasel 0.17.4 解出 rime.dll + 数据，补 wubi86
//!   cargo run --release          # 构建并运行 spike
//! 注意：vendor/ 已 gitignore（含第三方二进制）；spike 独立 workspace，不进主仓库 CI。

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};

type RimeBool = i32;
type RimeSessionId = usize; // librime 1.17+：uintptr_t（64 位），非 int

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
struct RimeCommit {
    data_size: i32,
    text: *mut c_char,
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
type FnGetCommit = unsafe extern "C" fn(session: RimeSessionId, commit: *mut RimeCommit) -> RimeBool;
type FnFreeCommit = unsafe extern "C" fn(commit: *mut RimeCommit) -> RimeBool;
type FnGetSchemaList = unsafe extern "C" fn(schema_list: *mut RimeSchemaList) -> RimeBool;
type FnFreeSchemaList = unsafe extern "C" fn(schema_list: *mut RimeSchemaList);
type FnGetStatus = unsafe extern "C" fn(session: RimeSessionId, status: *mut RimeStatus) -> RimeBool;
type FnFreeStatus = unsafe extern "C" fn(status: *mut RimeStatus) -> RimeBool;
type FnSelectSchema = unsafe extern "C" fn(session: RimeSessionId, schema_id: *const c_char) -> RimeBool;

/// 动态加载 rime.dll（显式 LoadLibrary/GetProcAddress，规避 raw-dylib 在
/// GNU 工具链下的运行时导入问题），返回函数指针集合。
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
    get_commit: FnGetCommit,
    free_commit: FnFreeCommit,
    get_schema_list: FnGetSchemaList,
    free_schema_list: FnFreeSchemaList,
    get_status: FnGetStatus,
    free_status: FnFreeStatus,
    select_schema: FnSelectSchema,
}

impl Rime {
    fn load(dll_path: &Path) -> Result<Self, String> {
        unsafe {
            let wide: Vec<u16> = dll_path
                .to_str()
                .ok_or("DLL 路径非 UTF-8")?
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let module = LoadLibraryW(wide.as_ptr());
            if module.is_null() {
                return Err(format!("LoadLibraryW 失败: {}", dll_path.display()));
            }
            let get = |name: &str| -> Result<*mut c_void, String> {
                let cname = CString::new(name).map_err(|e| e.to_string())?;
                let p = GetProcAddress(module, cname.as_ptr() as *const u8);
                if p.is_null() {
                    Err(format!("GetProcAddress 失败: {name}"))
                } else {
                    Ok(p)
                }
            };
            let rime = Rime {
                _module: module,
                setup_logging: std::mem::transmute(get("RimeSetupLogging")?),
                initialize: std::mem::transmute(get("RimeInitialize")?),
                finalize: std::mem::transmute(get("RimeFinalize")?),
                create_session: std::mem::transmute(get("RimeCreateSession")?),
                destroy_session: std::mem::transmute(get("RimeDestroySession")?),
                start_maintenance: std::mem::transmute(get("RimeStartMaintenance")?),
                join_maintenance_thread: std::mem::transmute(get("RimeJoinMaintenanceThread")?),
                simulate_key_sequence: std::mem::transmute(get("RimeSimulateKeySequence")?),
                get_commit: std::mem::transmute(get("RimeGetCommit")?),
                free_commit: std::mem::transmute(get("RimeFreeCommit")?),
                get_schema_list: std::mem::transmute(get("RimeGetSchemaList")?),
                free_schema_list: std::mem::transmute(get("RimeFreeSchemaList")?),
                get_status: std::mem::transmute(get("RimeGetStatus")?),
                free_status: std::mem::transmute(get("RimeFreeStatus")?),
                select_schema: std::mem::transmute(get("RimeSelectSchema")?),
            };
            Ok(rime)
        }
    }
}

fn cstring(s: &str) -> CString {
    CString::new(s).expect("无内嵌 NUL")
}

fn to_rust(ptr: *const c_char) -> String {
    if ptr.is_null() {
        "(null)".to_owned()
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

fn vendor_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor")
}

fn commit_once(rime: &Rime, session: RimeSessionId, label: &str) {
    unsafe {
        let mut commit = RimeCommit {
            data_size: std::mem::size_of::<RimeCommit>() as i32,
            text: std::ptr::null_mut(),
        };
        if (rime.get_commit)(session, &mut commit).as_bool() {
            let text = to_rust(commit.text);
            println!("[{label}] 上屏: {text:?}");
            (rime.free_commit)(&mut commit);
        } else {
            println!("[{label}] （无提交文本）");
        }
    }
}

fn schema_status(rime: &Rime, session: RimeSessionId, label: &str) {
    unsafe {
        let mut status = RimeStatus {
            data_size: std::mem::size_of::<RimeStatus>() as i32,
            schema_id: std::ptr::null_mut(),
            schema_name: std::ptr::null_mut(),
            is_disabled: 0,
            is_composing: 0,
            is_ascii_mode: 0,
            is_full_shape: 0,
            is_simplified: 0,
            is_traditional: 0,
            is_ascii_punct: 0,
        };
        let ok = (rime.get_status)(session, &mut status).as_bool();
        println!("[{label}] RimeGetStatus → {ok}");
        if ok {
            println!(
                "[{label}] schema={} ({})  composing={} ascii={}",
                to_rust(status.schema_id),
                to_rust(status.schema_name),
                status.is_composing,
                status.is_ascii_mode
            );
            (rime.free_status)(&mut status);
        }
    }
}

fn simulate(rime: &Rime, session: RimeSessionId, seq: &str, label: &str) {
    let seq = cstring(seq);
    unsafe {
        let ok = (rime.simulate_key_sequence)(session, seq.as_ptr()).as_bool();
        println!("[{label}] 模拟输入 {seq:?} → 返回 {ok}");
    }
    commit_once(rime, session, label);
}

impl Drop for Rime {
    fn drop(&mut self) {
        unsafe {
            FreeLibrary(self._module);
        }
    }
}

trait BoolExt {
    fn as_bool(self) -> bool;
}
impl BoolExt for RimeBool {
    fn as_bool(self) -> bool {
        self != 0
    }
}

fn main() {
    let vendor = vendor_dir();
    let shared = vendor.join("data");
    let user = vendor.join("user_data");
    std::fs::create_dir_all(&shared).expect("data 目录");
    std::fs::create_dir_all(&user).expect("user_data 目录");
    println!(
        "shared_data_dir={}\nuser_data_dir={}",
        shared.display(),
        user.display()
    );

    let log_dir = user.join("log");
    std::fs::create_dir_all(&log_dir).expect("log 目录");
    let shared_c = cstring(shared.to_str().unwrap());
    let user_c = cstring(user.to_str().unwrap());
    let app_c = cstring("verba.spike");
    let log_dir_c = cstring(log_dir.to_str().unwrap());

    let mut traits = RimeTraits {
        data_size: std::mem::size_of::<RimeTraits>() as i32,
        shared_data_dir: shared_c.as_ptr(),
        user_data_dir: user_c.as_ptr(),
        distribution_name: std::ptr::null(),
        distribution_code_name: std::ptr::null(),
        distribution_version: std::ptr::null(),
        app_name: app_c.as_ptr(),
        modules: std::ptr::null(),
        min_log_level: 0,
        log_dir: log_dir_c.as_ptr(),
        prebuilt_data_dir: std::ptr::null(),
        staging_dir: std::ptr::null(),
    };

    let rime = match Rime::load(&vendor.join("rime.dll")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("加载 rime.dll 失败: {e}");
            std::process::exit(1);
        }
    };

    unsafe {
        (rime.setup_logging)();
        (rime.initialize)(&mut traits);
        // 首次运行需要部署（编译 schema/词典）；同步等待完成
        println!("部署中…");
        (rime.start_maintenance)(0);
        (rime.join_maintenance_thread)();
        println!("部署完成");

        // 1) 可用方案列表
        let mut list = RimeSchemaList {
            size: 0,
            list: std::ptr::null_mut(),
        };
        if (rime.get_schema_list)(&mut list).as_bool() {
            println!("可用方案（{} 个）:", list.size);
            for i in 0..list.size {
                let item = &*list.list.add(i);
                println!("  - {} ({})", to_rust(item.schema_id), to_rust(item.name));
            }
            (rime.free_schema_list)(&mut list);
        } else {
            println!("获取方案列表失败");
        }

        // 2) 拼音（默认 luna_pinyin）
        let session = (rime.create_session)();
        if session == 0 {
            eprintln!("创建会话失败");
            (rime.finalize)();
            std::process::exit(1);
        }
        println!("[诊断] session_id={session}");
        schema_status(&rime, session, "拼音");
        simulate(&rime, session, "nishishui ", "nishishui");
        simulate(
            &rime,
            session,
            "jintianwanshangchishenme ",
            "今天 晚上 吃 什么",
        );
        simulate(&rime, session, "nihao ", "nihao");

        // 3) 五笔（wubi86）：你=wq 好=vb → wqvb 空格
        let sel = cstring("wubi86");
        let ok = (rime.select_schema)(session, sel.as_ptr()).as_bool();
        println!("[五笔] 选择 wubi86 → {ok}");
        schema_status(&rime, session, "五笔");
        simulate(&rime, session, "wqvb ", "你好");

        (rime.destroy_session)(session);
        (rime.finalize)();
    }
    println!("spike done");
}
