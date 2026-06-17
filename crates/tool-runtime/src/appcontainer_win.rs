//! Windows AppContainer 沙箱（M8.2 / Plan28 P3-10）——文件路径 + 网络隔离。
//!
//! AppContainer 令牌：进程以低完整性(Low IL)、默认拒绝文件/网络运行;可访问范围仅由
//! - 对工作区目录的显式 ACL 授权(P2,容器仅能读写工作区)
//! - 网络能力 SID 门控(P1,allow_network 决定能否出站)
//! 决定。命令经 powershell -EncodedCommand 包装、擦密钥环境块,与受限令牌沙箱一致。
//!
//! fail-closed：任一步骤失败返回 Err;由 run_command_streaming 决定是否在「该 Windows 版本不支持
//! AppContainer」时降级到受限令牌沙箱(M8.1),而非裸跑。
//!
//! 注:网络放行依赖宿主 Windows 防火墙开启(WFP 的能力放行过滤器由防火墙服务装载);防火墙全关时,
//! 即使 internetClient 能力已落入令牌,容器仍无法出站(默认拒绝侧不受影响,始终生效)。

#![cfg(windows)]

use crate::{CommandCancel, CommandLineCallback, RunCommandResult, ToolRuntimeError};
use std::ffi::c_void;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, BOOL, HANDLE, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetNamedSecurityInfoW, GetSecurityInfo, SetEntriesInAclW,
    SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    EqualSid, GetAce, InitializeSecurityDescriptor, SetKernelObjectSecurity,
    SetSecurityDescriptorDacl, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CONTAINER_INHERIT_ACE,
    DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
    OPEN_EXISTING,
};
use windows::Win32::Security::{
    FreeSid, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, PSID,
};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

fn err(msg: impl std::fmt::Display) -> ToolRuntimeError {
    ToolRuntimeError::CommandFailed(format!("AppContainer 沙箱: {msg}"))
}

/// 把宽字符串转成以 NUL 结尾的 UTF-16。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 构建容器用环境块:擦密钥 + 把 TEMP/TMP 重定向到容器可写的沙箱临时目录。
///
/// 必要性:AppContainer 默认拒绝用户 %TEMP%(AppData\Local\Temp 未授权);而 PowerShell 处理
/// 外部命令、.NET、众多工具(git/npm)都需要可写 TEMP。不重定向会导致外部命令静默无输出。
fn build_sandbox_env_block(temp_dir: &str) -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars() {
        let upper = k.to_uppercase();
        // 擦密钥(与受限令牌沙箱一致)。
        if upper.contains("API_KEY")
            || upper.contains("APIKEY")
            || upper.contains("SECRET")
            || upper.contains("_TOKEN")
            || upper.contains("PASSWORD")
        {
            continue;
        }
        // TEMP/TMP 由沙箱临时目录接管,丢弃父进程的值。
        if upper == "TEMP" || upper == "TMP" {
            continue;
        }
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.extend(format!("TEMP={temp_dir}").encode_utf16());
    block.push(0);
    block.extend(format!("TMP={temp_dir}").encode_utf16());
    block.push(0);
    block.push(0); // 双 NUL 结尾
    block
}

/// 创建或复用 AppContainer 容器 profile,返回其 SID（调用方负责 FreeSid）。
///
/// 首次用 CreateAppContainerProfile 创建;已存在(ERROR_ALREADY_EXISTS)时回落
/// DeriveAppContainerSidFromAppContainerName 复用。容器名应每会话隔离(name 含 sessionId 哈希)。
unsafe fn create_or_derive_appcontainer_sid(
    name: &str,
    display: &str,
    caps: Option<&[SID_AND_ATTRIBUTES]>,
) -> Result<PSID, ToolRuntimeError> {
    let wname = wide(name);
    let wdisp = wide(display);
    match CreateAppContainerProfile(
        PCWSTR(wname.as_ptr()),
        PCWSTR(wdisp.as_ptr()),
        PCWSTR(wdisp.as_ptr()),
        caps,
    ) {
        Ok(sid) => Ok(sid),
        Err(e) => {
            // 0x800700B7 = HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)：profile 已存在,改派生复用。
            if e.code().0 as u32 == 0x800700B7 {
                DeriveAppContainerSidFromAppContainerName(PCWSTR(wname.as_ptr()))
                    .map_err(|e| err(format!("DeriveAppContainerSid 失败: {e}")))
            } else {
                Err(err(format!("CreateAppContainerProfile 失败: {e}")))
            }
        }
    }
}

/// SID 的 RAII 守卫:Drop 时 FreeSid,确保任一早返回路径(grant/ConPTY/CreateProcess 失败)都不泄漏容器 SID。
struct SidGuard(PSID);
impl Drop for SidGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            unsafe { FreeSid(self.0) };
        }
    }
}

/// 能力 SID(ConvertStringSidToSidW 经 LocalAlloc 分配)的 RAII 守卫:Drop 时逐个 LocalFree,
/// 确保 grant/ConPTY 等早返回路径不泄漏。
struct LocalSidsGuard(Vec<PSID>);
impl Drop for LocalSidsGuard {
    fn drop(&mut self) {
        for sid in &self.0 {
            if !sid.0.is_null() {
                unsafe {
                    let _ = LocalFree(HLOCAL(sid.0));
                }
            }
        }
    }
}

/// 目录是否为重解析点(junction/symlink)。向 reparse 点授「读写」会经其 target 逃逸出工作区,故拒绝。
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_attributes() & 0x0000_0400 != 0) // FILE_ATTRIBUTE_REPARSE_POINT
        .unwrap_or(false)
}

/// grant_dir_access_to_appcontainer 实际写回 DACL(SetNamedSecurityInfoW)的累计次数——幂等跳过命中时
/// 不增。供测试验证「第二次起跳过重写」,也是写放大的可观测指标。
pub(crate) static GRANT_WRITE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 检查 DACL 是否已含「指向 ac_sid、非继承、可继承(OI|CI)、含读写执行」的 allow ACE。命中说明工作区
/// 先前已授权过本容器(容器 SID 跨进程稳定),可跳过重写——避免每命令重写整棵子树 SD(改子文件 ChangeTime
/// 会误触发 tauri dev / vite 等文件监视器 rebuild,对大 repo 还每命令卡顿)。
unsafe fn dacl_has_inheritable_grant(dacl: *mut ACL, ac_sid: PSID) -> bool {
    if dacl.is_null() {
        return false;
    }
    let want_mask = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | FILE_GENERIC_EXECUTE.0;
    let want_inherit = OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0;
    let count = (*dacl).AceCount;
    for i in 0..count {
        let mut ace_ptr: *mut c_void = std::ptr::null_mut();
        if GetAce(dacl, i as u32, &mut ace_ptr).is_err() {
            continue;
        }
        let header = ace_ptr as *const ACE_HEADER;
        if (*header).AceType != 0 {
            continue; // 非 ACCESS_ALLOWED_ACE_TYPE(=0)
        }
        if (*header).AceFlags & 0x10 != 0 {
            continue; // INHERITED_ACE:继承来的不算根上显式授权
        }
        if (*header).AceFlags as u32 & want_inherit != want_inherit {
            continue; // 须同时含 OBJECT_INHERIT|CONTAINER_INHERIT
        }
        let aaa = ace_ptr as *const ACCESS_ALLOWED_ACE;
        if (*aaa).Mask & want_mask != want_mask {
            continue;
        }
        let sid_in_ace = PSID(&(*aaa).SidStart as *const u32 as *mut c_void);
        if EqualSid(sid_in_ace, ac_sid).is_ok() {
            return true;
        }
    }
    false
}

/// 给目录的 DACL 追加一条「允许容器 SID 读/写/执行(遍历)」的 ACE,使 AppContainer 命令能访问工作区。
///
/// 机制:AppContainer 默认拒绝;授权 = GetNamedSecurityInfoW 取现有 DACL → SetEntriesInAclW 合并新 ACE
/// (GRANT_ACCESS 追加,不替换) → SetNamedSecurityInfoW 写回。继承标志使子目录/文件一并可访问。
/// 注:spike 阶段暂未释放安全描述符/新 DACL(每命令小泄漏),集成时补 LocalFree。
unsafe fn grant_dir_access_to_appcontainer(path: &Path, ac_sid: PSID) -> Result<(), ToolRuntimeError> {
    // junction/symlink 逃逸防护:授读写给重解析点会经 target 逃出工作区。fail-closed 拒绝。
    if is_reparse_point(path) {
        return Err(err(format!(
            "拒绝向重解析点(junction/symlink)授读写权,防逃逸出工作区: {}",
            path.display()
        )));
    }
    let mut wpath = wide(&path.as_os_str().to_string_lossy());

    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let rc = GetNamedSecurityInfoW(
        PCWSTR(wpath.as_ptr()),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        None,
        None,
        Some(&mut old_dacl),
        None,
        &mut sd,
    );
    if rc.0 != 0 {
        return Err(err(format!("GetNamedSecurityInfo 失败: {}", rc.0)));
    }

    // 幂等跳过:工作区根已含本容器 SID 的可继承读写执行 ACE(先前命令已授权)→ 直接返回,不重写子树。
    // 关键修复:否则每命令都 SetNamedSecurityInfoW 重写整棵子树 SD,改子文件 ChangeTime 误触发
    // tauri dev/vite 等监视器 rebuild(dev 下应用反复重启),对大 repo 还每命令卡几十秒。
    if dacl_has_inheritable_grant(old_dacl, ac_sid) {
        if !sd.0.is_null() {
            let _ = LocalFree(HLOCAL(sd.0));
        }
        return Ok(());
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | FILE_GENERIC_EXECUTE.0,
        grfAccessMode: SET_ACCESS, // 幂等:替换该容器 SID 现有 ACE,多命令不累积
        grfInheritance: windows::Win32::Security::ACE_FLAGS(
            OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0,
        ),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            // TRUSTEE_IS_SID 形态下 ptstrName 承载 SID 指针。
            ptstrName: PWSTR(ac_sid.0 as *mut u16),
        },
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl as *const ACL), &mut new_dacl);
    if rc.0 != 0 {
        return Err(err(format!("SetEntriesInAcl 失败: {}", rc.0)));
    }

    let rc = SetNamedSecurityInfoW(
        PWSTR(wpath.as_mut_ptr()),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        PSID::default(),
        PSID::default(),
        Some(new_dacl as *const ACL),
        None,
    );
    // SetNamedSecurityInfo 会拷贝 DACL,写回后即可释放新 DACL 与原安全描述符。
    let _ = LocalFree(HLOCAL(new_dacl as *mut c_void));
    if !sd.0.is_null() {
        let _ = LocalFree(HLOCAL(sd.0));
    }
    if rc.0 != 0 {
        return Err(err(format!("SetNamedSecurityInfo 失败: {}", rc.0)));
    }
    GRANT_WRITE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 给目录授「仅遍历(FILE_TRAVERSE)、不继承」的 ACE：让容器能穿过此目录到达更深的已授权目录,
/// 但不能列此目录内容、也不传播给子项。用于给工作区父链开路——否则 PowerShell `Set-Location`
/// 到用户 profile 路径下的工作区会「拒绝访问」(AppContainer 默认对用户目录链遍历受限)。
unsafe fn grant_traverse_to_appcontainer(path: &Path, ac_sid: PSID) -> Result<(), ToolRuntimeError> {
    let wpath = wide(&path.as_os_str().to_string_lossy());
    // 打开目录句柄(READ_CONTROL|WRITE_DAC 才能读/改 DACL;BACKUP_SEMANTICS 才能以句柄打开目录对象)。
    let h = CreateFileW(
        PCWSTR(wpath.as_ptr()),
        0x0002_0000u32 | 0x0004_0000u32, // READ_CONTROL | WRITE_DAC
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )
    .map_err(|e| err(format!("打开目录句柄(traverse) 失败: {e}")))?;

    // 读原 DACL(GetSecurityInfo 是读操作,不传播)。
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let rc = GetSecurityInfo(
        h,
        SE_KERNEL_OBJECT,
        DACL_SECURITY_INFORMATION,
        None,
        None,
        Some(&mut old_dacl),
        None,
        Some(&mut sd),
    );
    if rc.0 != 0 {
        let _ = CloseHandle(h);
        return Err(err(format!("GetSecurityInfo(traverse) 失败: {}", rc.0)));
    }

    // 追加「仅遍历(FILE_TRAVERSE)、不继承」的 ACE。
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_TRAVERSE.0,
        grfAccessMode: SET_ACCESS, // 幂等:替换该容器 SID 现有 ACE
        grfInheritance: windows::Win32::Security::ACE_FLAGS(0), // 不继承
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(ac_sid.0 as *mut u16),
        },
    };
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl as *const ACL), &mut new_dacl);
    if rc.0 != 0 {
        if !sd.0.is_null() {
            let _ = LocalFree(HLOCAL(sd.0));
        }
        let _ = CloseHandle(h);
        return Err(err(format!("SetEntriesInAcl(traverse) 失败: {}", rc.0)));
    }

    // 关键:用 SetKernelObjectSecurity(走 NtSetSecurityObject)写回——它只设该目录对象「自身」的 DACL,
    // 绝不向子树传播继承。而 SetNamedSecurityInfo(advapi32)写带可继承 ACE 的 DACL 时会枚举并重写
    // 全部子项,对 %TEMP% 这种海量子项目录会卡数十秒(实测父链此处 54s)。父链只需目录自身可遍历,无需
    // 传播给子项,故底层 API 既正确又快。
    let mut new_sd = SECURITY_DESCRIPTOR::default();
    let psd = PSECURITY_DESCRIPTOR(&mut new_sd as *mut _ as *mut c_void);
    let set_res = InitializeSecurityDescriptor(psd, 1) // SECURITY_DESCRIPTOR_REVISION
        .and_then(|_| {
            SetSecurityDescriptorDacl(psd, BOOL(1), Some(new_dacl as *const ACL), BOOL(0))
        })
        .and_then(|_| SetKernelObjectSecurity(h, DACL_SECURITY_INFORMATION, psd));

    let _ = LocalFree(HLOCAL(new_dacl as *mut c_void));
    if !sd.0.is_null() {
        let _ = LocalFree(HLOCAL(sd.0));
    }
    let _ = CloseHandle(h);
    set_res.map_err(|e| err(format!("SetKernelObjectSecurity(traverse) 失败: {e}")))?;
    Ok(())
}

/// 把 ConPTY 输出的 VT/ANSI 转义流剥离为纯文本:去掉 CSI(ESC[..final)、OSC(ESC]..BEL/ST)、
/// 其它 ESC 序列;`\r\n` 归一为 `\n`,丢弃裸 `\r`(伪控制台的光标回车)。
pub(crate) fn strip_vt(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' {
            match chars.get(i + 1) {
                Some('[') => {
                    // CSI: ESC [ ...参数/中间字节... final(0x40..=0x7e)
                    i += 2;
                    while i < chars.len() {
                        let f = chars[i];
                        i += 1;
                        if ('\u{40}'..='\u{7e}').contains(&f) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] ... (BEL | ESC \)
                    i += 2;
                    while i < chars.len() {
                        if chars[i] == '\u{07}' {
                            i += 1;
                            break;
                        }
                        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2, // 其它 ESC x:跳过两字符
                None => i += 1,
            }
            continue;
        }
        if c == '\r' {
            i += 1; // 丢弃裸 \r(\r\n 的 \n 下一轮保留)
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 在 AppContainer 中执行命令并捕获输出。隔离层次:真 AppContainer 令牌(Low IL + 默认拒绝文件/网络)
/// + 工作区 ACL 授权(容器仅能读写工作区) + 能力 SID 网络门控(allow_network 决定能否出站)。
///
/// 输出走 ConPTY 伪控制台:配合 LowBoxConsoleEnabled + lpacCom/registryRead 能力,容器内 native console
/// 程序(git/npm/cmd 等)的输出才能回传(纯管道在 AppContainer 内对 native 孙进程会丢)。命令前用
/// New-PSDrive 把工作区挂成 PSDrive 并 Set-Location 进去——规避 AppContainer 下 Set-Location 到用户
/// profile 路径的「拒绝访问」,使 native cwd 与 cmdlet 相对路径都落在工作区。输出为 VT 流,strip_vt 还原。
///
/// fail-closed:任一步骤失败返回 Err,由 run_command_streaming 决定是否降级到受限令牌沙箱。
pub fn run_in_appcontainer_sandbox(
    workspace: &Path,
    command: &str,
    timeout: Duration,
    on_line: Option<CommandLineCallback>,
    cancel: Option<CommandCancel>,
    allow_network: bool,
) -> Result<RunCommandResult, ToolRuntimeError> {
    unsafe {
        // 能力:lpacCom(放行 native console 子进程连 conhost,使其输出经 ConPTY 回传——这是容器内
        // native 输出能回传的关键,不需改任何系统注册表)(+ allow_network 时加互联网能力)。能力须在
        // profile 创建期与进程启动期都登记,仅启动期传入不会真正落入令牌。
        let mut cap_sids: Vec<PSID> = Vec::new();
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        let mut cap_strs: Vec<&str> = vec![
            "S-1-15-3-1024-2405443489-874036122-4286035555-1823921565-1746547431-2453885448-3625952902-991631256", // lpacCom
        ];
        if allow_network {
            cap_strs.extend(["S-1-15-3-1", "S-1-15-3-2", "S-1-15-3-3"]);
        }
        for s in &cap_strs {
            let sw = wide(s);
            let mut sid = PSID::default();
            if ConvertStringSidToSidW(PCWSTR(sw.as_ptr()), &mut sid).is_ok() {
                cap_sids.push(sid);
                caps.push(SID_AND_ATTRIBUTES {
                    Sid: sid,
                    Attributes: 0x0000_0004, // SE_GROUP_ENABLED
                });
            }
        }
        let caps_arg: Option<&[SID_AND_ATTRIBUTES]> =
            if caps.is_empty() { None } else { Some(&caps[..]) };
        // 能力 SID(LocalAlloc)用 RAII 守卫:任一早返回也释放(caps 已存 PSID 副本,不受影响)。
        let _cap_guard = LocalSidsGuard(std::mem::take(&mut cap_sids));
        // 容器名按 workspace + 网络模式唯一:每工作区独立 SID/profile/ACL,不跨工作区累积权限。
        // 网络模式入名避免「已存在但能力集不符」的派生陷阱。DefaultHasher::new() 确定性(固定 key),
        // 同工作区跨进程稳定 → profile 可复用。
        let ws_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            workspace.as_os_str().hash(&mut h);
            h.finish()
        };
        let net_tag = if allow_network { "n" } else { "d" };
        let name = format!("MDGA.Sbx.{ws_hash:016x}.{net_tag}");
        let display = format!("MDGA sandbox {ws_hash:08x} ({net_tag})");

        // profile 创建 + ACL 授权用进程级锁串行化:避免并发命令对同目录 SetSecurityInfo / 同名 profile
        // 创建竞争(命令「执行」阶段不持锁,仍并行)。
        static SETUP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let setup_guard = SETUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SID 用 RAII 守卫:任一早返回(grant/ConPTY/CreateProcess 失败)都不泄漏。
        let _ac_guard = SidGuard(create_or_derive_appcontainer_sid(&name, &display, caps_arg)?);
        let ac_sid = _ac_guard.0;
        // 授权工作区目录(容器仅能读写工作区)+ 工作区父链 traverse(New-PSDrive 解析工作区路径需要)。
        grant_dir_access_to_appcontainer(workspace, ac_sid)?;
        let mut anc = workspace.parent();
        while let Some(dir) = anc {
            let _ = grant_traverse_to_appcontainer(dir, ac_sid);
            anc = dir.parent();
        }
        // 容器专用临时目录:按命令唯一(避免共享累积 + 并发冲突),收尾删除。接管 TEMP/TMP(容器读不到
        // 用户 %TEMP%,而 PowerShell/.NET/工具需可写 TEMP)。
        static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp_seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let sandbox_temp =
            std::env::temp_dir().join(format!("mdga-sbx-{}-{}", std::process::id(), temp_seq));
        let _ = std::fs::create_dir_all(&sandbox_temp);
        grant_dir_access_to_appcontainer(&sandbox_temp, ac_sid)?;
        let sandbox_temp_str = sandbox_temp.as_os_str().to_string_lossy().to_string();

        // profile + ACL 已完成,释放 setup 锁;后续 ConPTY/CreateProcess/命令执行并行进行。
        drop(setup_guard);

        let sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: ac_sid,
            Capabilities: if caps.is_empty() {
                std::ptr::null_mut()
            } else {
                caps.as_mut_ptr()
            },
            CapabilityCount: caps.len() as u32,
            Reserved: 0,
        };

        // ConPTY 管道:in(我们写→PTY)、out(PTY→我们读)。
        let (mut in_r, mut in_w) = (HANDLE::default(), HANDLE::default());
        let (mut out_r, mut out_w) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_r, &mut in_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(in) 失败: {e}")))?;
        CreatePipe(&mut out_r, &mut out_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(out) 失败: {e}")))?;
        let coord = COORD { X: 160, Y: 50 };
        let hpc: HPCON = CreatePseudoConsole(coord, in_r, out_w, 0)
            .map_err(|e| err(format!("CreatePseudoConsole 失败: {e}")))?;
        let _ = CloseHandle(in_r);
        let _ = CloseHandle(out_w);

        // 属性列表:SECURITY_CAPABILITIES + PSEUDOCONSOLE。
        let mut attr_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            2,
            0,
            &mut attr_size,
        );
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(attr_list, 2, 0, &mut attr_size)
            .map_err(|e| err(format!("InitializeProcThreadAttributeList 失败: {e}")))?;
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some(&sec_caps as *const _ as *const c_void),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )
        .map_err(|e| err(format!("UpdateProcThreadAttribute(caps) 失败: {e}")))?;
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE_VAL,
            Some(hpc.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(|e| err(format!("UpdateProcThreadAttribute(pty) 失败: {e}")))?;

        // STARTUPINFOEXW:NULL std 句柄,强制子进程用伪控制台(不经 PEB 继承父进程的重定向句柄)。
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = HANDLE::default();
        si.StartupInfo.hStdOutput = HANDLE::default();
        si.StartupInfo.hStdError = HANDLE::default();
        si.lpAttributeList = attr_list;

        // 命令包装:New-PSDrive 把工作区挂成 PSDrive 并 Set-Location 进去,再跑用户命令。
        let ws_root = workspace.as_os_str().to_string_lossy().replace('\'', "''");
        let wrapped = format!(
            "New-PSDrive -Name MWS -PSProvider FileSystem -Root '{ws}' -Scope Global -ErrorAction SilentlyContinue | Out-Null; Set-Location MWS: -ErrorAction SilentlyContinue; {cmd}",
            ws = ws_root,
            cmd = command
        );
        let mut cmdline = crate::sandbox_win::encoded_command_line(&wrapped);
        let mut env_block = build_sandbox_env_block(&sandbox_temp_str);
        let cwd = wide(&workspace.as_os_str().to_string_lossy());

        // Job Object:KILL_ON_JOB_CLOSE 保证进程树(含 native 孙进程)随 job 关闭干净销毁——既杜绝孤儿,
        // 又让超时/cancel 后 PTY 写端随之全关、reader 不再永久阻塞;加进程数/内存配额防 fork-bomb / 内存炸。
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|e| err(format!("CreateJobObject 失败: {e}")))?;
        let mut jinfo = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        jinfo.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_JOB_MEMORY;
        jinfo.BasicLimitInformation.ActiveProcessLimit = 256; // 进程数上限
        jinfo.JobMemoryLimit = 4 * 1024 * 1024 * 1024; // 4GiB 进程树总内存上限
        let _ = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &jinfo as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );

        let mut pi = PROCESS_INFORMATION::default();
        let create_result = CreateProcessW(
            PCWSTR::null(),
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            false, // ConPTY:句柄经伪控制台传入,不靠继承
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            Some(env_block.as_mut_ptr() as *const c_void),
            PCWSTR(cwd.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attr_list);
        if let Err(e) = create_result {
            let _ = CloseHandle(out_r);
            let _ = CloseHandle(in_w);
            let _ = CloseHandle(job);
            ClosePseudoConsole(hpc);
            return Err(err(format!("CreateProcess(AppContainer/ConPTY) 失败: {e}")));
        }
        // 挂进 job 后再 resume:子进程一启动就在 job 内,它 fork 的孙进程也自动归 job。
        let _ = AssignProcessToJobObject(job, pi.hProcess);
        ResumeThread(pi.hThread);

        // 读伪控制台输出(VT 流)的线程:增量把 chunk 发回主线程(不 join,避免永久阻塞在 read)。
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let out_raw = out_r.0 as isize;
        let on_line_reader = on_line.clone();
        std::thread::spawn(move || {
            let mut f = std::fs::File::from_raw_handle(out_raw as *mut c_void);
            let mut chunk = [0u8; 8192];
            // 流式:按 \n 切行缓冲(跨 chunk),每整行经 strip_vt 后实时 on_line 回调。行内 VT 转义完整、
            // \n 不会切断多字节 UTF-8,故逐行用纯函数 strip_vt 即可,无需流式状态机。
            let mut line_buf: Vec<u8> = Vec::new();
            loop {
                match std::io::Read::read(&mut f, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // 仍把原始 chunk 发回主线程收集 raw(供最终 result.stdout)。
                        if tx.send(chunk[..n].to_vec()).is_err() {
                            break;
                        }
                        if let Some(cb) = on_line_reader.as_ref() {
                            for &b in &chunk[..n] {
                                if b == b'\n' {
                                    cb(strip_vt(&String::from_utf8_lossy(&line_buf)));
                                    line_buf.clear();
                                } else {
                                    line_buf.push(b);
                                }
                            }
                        }
                    }
                }
            }
            // 收尾:emit 无 \n 结尾的最后一行尾巴。
            if let Some(cb) = on_line_reader.as_ref() {
                if !line_buf.is_empty() {
                    let line = strip_vt(&String::from_utf8_lossy(&line_buf));
                    if !line.is_empty() {
                        cb(line);
                    }
                }
            }
        });

        // 等子进程退出 / cancel / 超时(WaitForSingleObject 50ms 粒度)。超时后由下方 TerminateJobObject
        // 杀整个进程树兜底。
        let started = Instant::now();
        let mut timed_out = false;
        loop {
            if WaitForSingleObject(pi.hProcess, 50) == WAIT_OBJECT_0 {
                break;
            }
            if cancel.as_ref().map(|c| c.load(Ordering::SeqCst)).unwrap_or(false) {
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                break;
            }
        }
        let mut exit_code: u32 = 0;
        let _ = GetExitCodeProcess(pi.hProcess, &mut exit_code);
        // 杀整个进程树(含 native 孙进程)。
        let _ = TerminateJobObject(job, 1);
        let _ = CloseHandle(in_w);
        // ClosePseudoConsole 会阻塞到所有客户端(含 ConPTY 自己的 conhost,不在 job 内)退出;放 detached
        // 线程,避免某进程不退时卡死主线程。它生效后会关掉 out_w → reader 读到 EOF。
        let hpc_raw = hpc.0 as isize;
        std::thread::spawn(move || {
            ClosePseudoConsole(HPCON(hpc_raw));
        });
        // 收集 PTY 输出:reader 因 out_r EOF 正常结束(发送端 drop→Disconnected),或 3s 无新数据兜底(绝不永久挂)。
        let mut raw = Vec::new();
        let read_deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if Instant::now() >= read_deadline {
                break; // 收尾总体兜底:进程已结束,最多再等 3s 排空 PTY,绝不永久挂(含持续输出场景)
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => raw.extend_from_slice(&c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(job);
        let _ = std::fs::remove_dir_all(&sandbox_temp); // 删本命令专属临时目录
        let duration_ms = started.elapsed().as_millis();

        // VT 流剥离为纯文本,截断到 64KiB(char 边界);按行触发 on_line 回调。
        let mut text = strip_vt(&String::from_utf8_lossy(&raw));
        let mut truncated = false;
        const MAX: usize = 64 * 1024;
        if text.len() > MAX {
            let mut end = MAX;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        // on_line 已由 reader 线程在命令运行期间实时逐行回调(流式),此处不再重复;text 仅填 result.stdout。

        Ok(RunCommandResult {
            command: command.to_string(),
            exit_code: if timed_out { None } else { Some(exit_code as i32) },
            stdout: text,
            stderr: String::new(), // ConPTY 合并 stdout/stderr 到单一终端流
            truncated,
            timed_out,
            duration_ms,
            sandbox_layer: Some("appcontainer".to_string()),
            sandbox_degraded: false,
        })
    }
}

/// 运行时能力自检:在临时区跑探针,实测**本机** AppContainer 方案能否回传 native console 输出。
///
/// 各版本 Windows 对 LowBoxConsoleEnabled / ConPTY / AppContainer / lpacCom 的支持不一(我们只在
/// 一台 Win11 上验证过),与其按版本号猜,不如实跑一条 `cmd echo <token>` 看输出是否回传——回传则
/// AppContainer 全功能可用,否则由调用方 fail-closed 降级到受限令牌沙箱。结果应由调用方缓存(每进程
/// 探一次即可)。探针自带 ensure_lowbox_console_enabled(经 run_in_appcontainer_sandbox),故也顺带
/// 验证了注册表开关在本机是否真生效。
pub fn appcontainer_self_test() -> bool {
    let probe_ws = std::env::temp_dir().join("mdga-ac-selftest");
    if std::fs::create_dir_all(&probe_ws).is_err() {
        return false;
    }
    // nonce(纳秒时戳)防误判:避免历史/缓存输出里残留的 token 被当成探针成功。
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("MDGA_SELFTEST_{nonce:x}");
    let r = run_in_appcontainer_sandbox(
        &probe_ws,
        &format!("cmd /c echo {token}"),
        Duration::from_secs(15),
        None,
        None,
        false,
    );
    let _ = std::fs::remove_dir_all(&probe_ws);
    matches!(r, Ok(o) if o.stdout.contains(token.as_str()))
}

/// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE 的原始值(windows 0.58 未必导出常量,直接用数值)。
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE_VAL: usize = 0x0002_0016;

#[cfg(test)]
mod tests {
    use super::*;

    /// AppContainer 测试共享机器级全局状态(容器 profile、共享沙箱 temp 目录的 DACL),
    /// 并行会相互踩踏(授权读改写竞态、profile 并发创建)。用此锁串行化各容器测试。
    static SANDBOX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        SANDBOX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn strip_vt_removes_escapes() {
        assert_eq!(strip_vt("\u{1b}[?25l\u{1b}[2Jhello\u{1b}[m"), "hello");
        assert_eq!(strip_vt("\u{1b}]0;title\u{07}world"), "world");
        assert_eq!(strip_vt("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(strip_vt("plain text 42"), "plain text 42");
        assert_eq!(strip_vt("\u{1b}]0;t\u{1b}\\X"), "X");
    }

    /// 运行时自检在本机应通过(方案B可用)——这是 dispatch 决定走 AppContainer 还是降级受限令牌的依据。
    #[test]
    fn appcontainer_self_test_passes_here() {
        let _g = test_guard();
        assert!(
            appcontainer_self_test(),
            "本机 AppContainer 自检未通过——dispatch 会降级受限令牌(若预期可用,查 LowBoxConsole/能力/ConPTY)"
        );
    }

    /// Job Object 应让超时/cancel 后及时返回(不挂死):跑一个持续运行的 native 命令(waitfor 等永不到来的
    /// 信号),设短 timeout;验证 Job 杀进程树后 PTY 写端全关、reader 立即 EOF、调用及时返回(不卡在 join)。
    #[test]
    fn appcontainer_timeout_does_not_hang() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-to-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let start = Instant::now();
        let r = run_in_appcontainer_sandbox(
            &ws,
            "Start-Sleep -Seconds 8",
            Duration::from_secs(3),
            None,
            None,
            false,
        );
        let elapsed = start.elapsed();
        let _ = std::fs::remove_dir_all(&ws);
        let out = r.expect("起进程失败");
        eprintln!("[timeout] timed_out={} elapsed={:?}", out.timed_out, elapsed);
        assert!(out.timed_out, "应判超时");
        assert!(
            elapsed < Duration::from_secs(15),
            "超时后应及时返回(Job 杀进程树→ PTY EOF),实际 {:?} 说明 reader 挂死",
            elapsed
        );
    }

    /// 生产路径端到端:run_in_appcontainer_sandbox(ConPTY + lpacCom 能力 + New-PSDrive)
    /// 跑 native 命令,验证(1)在工作区 cwd 跑(相对路径读到工作区文件) (2)native 输出经伪控制台回传、
    /// 已 strip_vt 成纯文本。这是方案 B 完整接通的回归测试。
    #[test]
    fn appcontainer_sandbox_runs_native_in_workspace() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-prod-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("hello.txt"), "PRODFILE_88").unwrap();
        let r = run_in_appcontainer_sandbox(
            &ws,
            "cmd /c type hello.txt",
            Duration::from_secs(30),
            None,
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&ws);
        let out = r.expect("起进程失败(环境可能不支持 AppContainer)");
        eprintln!("[prod] exit={:?} stdout=[{}]", out.exit_code, out.stdout.trim());
        assert!(
            out.stdout.contains("PRODFILE_88"),
            "生产沙箱:native 命令未在工作区读到文件/输出未回传:\n{}",
            out.stdout
        );
        assert_eq!(
            out.sandbox_layer.as_deref(),
            Some("appcontainer"),
            "可观测:AppContainer 路径应标记 sandbox_layer=appcontainer"
        );
        assert!(!out.sandbox_degraded, "AppContainer 成功不应标记降级");
    }

    /// 输出实时化(0.0.42):on_line 在命令运行期间逐行回调(reader 流式),验证多行命令每行都收到。
    #[test]
    fn appcontainer_streams_lines_via_on_line() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-stream-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("multi.txt"), "AAA\r\nBBB\r\nCCC\r\n").unwrap();
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let cb: CommandLineCallback = std::sync::Arc::new(move |line: String| {
            sink.lock().unwrap().push(line);
        });
        let r = run_in_appcontainer_sandbox(
            &ws,
            "cmd /c type multi.txt",
            Duration::from_secs(30),
            Some(cb),
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _out = r.expect("起进程失败");
        let collected = lines.lock().unwrap().join("\n");
        assert!(collected.contains("AAA"), "on_line 应逐行收到 AAA:\n{collected}");
        assert!(collected.contains("BBB"), "on_line 应逐行收到 BBB:\n{collected}");
        assert!(collected.contains("CCC"), "on_line 应逐行收到 CCC:\n{collected}");
    }

    /// junction/symlink 逃逸防护:工作区是重解析点时,grant 阶段 fail-closed 拒绝(防经 target 逃出工作区)。
    #[test]
    fn appcontainer_rejects_reparse_point_workspace() {
        let _g = test_guard();
        let pid = std::process::id();
        let real = std::env::temp_dir().join(format!("mdga-real-{pid}"));
        let link = std::env::temp_dir().join(format!("mdga-junc-{pid}"));
        std::fs::create_dir_all(&real).unwrap();
        let _ = std::fs::remove_dir_all(&link);
        // mklink /J 建 junction(普通用户即可,无需管理员)。
        let made = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &link.to_string_lossy(),
                &real.to_string_lossy(),
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            let _ = std::fs::remove_dir_all(&real);
            eprintln!("[reparse] 无法建 junction,跳过");
            return;
        }
        assert!(is_reparse_point(&link), "junction 应被识别为 reparse point");
        let r = run_in_appcontainer_sandbox(
            &link,
            "cmd /c echo x",
            Duration::from_secs(20),
            None,
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&link);
        let _ = std::fs::remove_dir_all(&real);
        let e = r.expect_err("对 reparse 工作区应 fail-closed 拒绝");
        let msg = format!("{e}");
        assert!(
            msg.contains("重解析点"),
            "错误应说明拒绝 reparse 授权,实际: {msg}"
        );
    }

    /// 同工作区重复跑:验证容器名复用 + SET_ACCESS 幂等(ACE 不累积)+ SidGuard 不泄漏 + temp 收尾删除。
    /// 多次跑都应稳定成功、读到工作区文件,且本进程的 mdga-sbx-* 临时目录不残留累积。
    #[test]
    fn appcontainer_repeat_same_workspace_idempotent() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-rep-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("rep.txt"), "REP_77").unwrap();
        for i in 0..3 {
            let out = run_in_appcontainer_sandbox(
                &ws,
                "cmd /c type rep.txt",
                Duration::from_secs(30),
                None,
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("第 {i} 次起进程失败: {e}"));
            assert!(
                out.stdout.contains("REP_77"),
                "第 {i} 次未读到工作区文件:\n{}",
                out.stdout
            );
        }
        let _ = std::fs::remove_dir_all(&ws);
        // 各命令专属临时目录应已在各自收尾删除,不累积残留。
        let prefix = format!("mdga-sbx-{}-", std::process::id());
        let leaked = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(leaked, 0, "命令专属临时目录未清理,残留 {leaked} 个");
    }

    /// 幂等跳过(闪退修复):同工作区第二条命令应跳过 workspace 的 ACL 重写,只剩每命令新建的 temp 写回。
    /// 这是"每命令重写工作区子树 SD → 误触发 dev 文件监视器 rebuild"的直接验证。
    #[test]
    fn grant_dir_access_skips_rewrite_when_already_granted() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-grant-skip-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("f.txt"), "x").unwrap();
        use std::sync::atomic::Ordering;
        let c0 = GRANT_WRITE_COUNT.load(Ordering::Relaxed);
        run_in_appcontainer_sandbox(&ws, "cmd /c echo a", Duration::from_secs(30), None, None, false)
            .expect("第 1 次起进程失败");
        let c1 = GRANT_WRITE_COUNT.load(Ordering::Relaxed);
        run_in_appcontainer_sandbox(&ws, "cmd /c echo b", Duration::from_secs(30), None, None, false)
            .expect("第 2 次起进程失败");
        let c2 = GRANT_WRITE_COUNT.load(Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&ws);
        // 第 1 次:workspace + sandbox_temp 各写回一次 = +2;第 2 次:workspace 命中跳过、仅 temp 写回 = +1。
        assert_eq!(c1 - c0, 2, "首次应写回 workspace + temp 两处 ACL");
        assert_eq!(c2 - c1, 1, "第二次 workspace 应跳过(幂等),只剩 temp 写回");
    }

    /// P0 spike：在 token+AppContainer 内跑 `whoami /groups`,断言进程能起、且完整性级别=Low、
    /// 且令牌组含 AppContainer SID(S-1-15-2-...)。这是 AppContainer 真生效的最直接证据。
    /// 在目标 Windows build 上真机运行;若环境不支持(CreateAppContainerProfile 拒绝),测试会失败并打印原因。
    #[test]
    fn spike_functionally_blocks_reading_outside_workspace() {
        let _g = test_guard();
        // 功能性证明(设计 #6 首选):沙箱进程读一个**工作区外**的真实文件。
        // AppContainer 真生效 → 默认拒绝 → 读不到内容;若仅 Low IL 而无容器隔离 → 能读到(Low IL 不挡读)。
        // 用本仓库 Cargo.toml 作探针目标(普通进程可读、含已知标记 [workspace]);workspace 传 temp(未授权)。
        let target = "C:\\Users\\AIT\\Desktop\\NovaStudio\\MDGA\\Cargo.toml";
        // 先确认该文件普通可读(夹具有效),否则测试无意义。
        let normally = std::fs::read_to_string(target).unwrap_or_default();
        assert!(
            normally.contains("[workspace]"),
            "夹具失效:探针目标文件不可读或无预期标记,换一个稳定的工作区外文件"
        );

        let tmp = std::env::temp_dir();
        let r = run_in_appcontainer_sandbox(
            &tmp,
            &format!("C:\\Windows\\System32\\cmd.exe /c type \"{target}\""),
            Duration::from_secs(20),
            None,
            None,
            false,
        );
        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("spike 起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        let text = format!("{}\n{}", out.stdout, out.stderr);
        eprintln!("--- type 工作区外文件 (exit={:?}) ---\n{}", out.exit_code, text);
        // 隔离生效的判据:输出里**不应**出现该文件的已知内容标记。
        assert!(
            !text.contains("[workspace]"),
            "AppContainer 未隔离文件系统:沙箱进程读到了工作区外文件的内容(只 Low IL、容器未真正生效):\n{text}"
        );
    }

    /// P2:工作区 ACL 授权后,沙箱进程**应能**读到工作区内文件(否则默认拒绝、命令完全不可用)。
    /// 与上一测试合起来构成完整证明:工作区内可读(已授权)、工作区外不可读(隔离生效)。
    #[test]
    fn appcontainer_can_read_inside_granted_workspace() {
        let _g = test_guard();
        // 用独立子目录当工作区,避免污染 temp 根的 DACL、也避免与其他测试串味。
        let ws = std::env::temp_dir().join(format!("mdga-ac-ws-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        let marker = "[mdga-inside-marker-77]";
        let file = ws.join("inside.txt");
        std::fs::write(&file, marker).expect("写工作区内探针文件失败");

        let r = run_in_appcontainer_sandbox(
            &ws,
            &format!("Get-Content -Raw '{}'", file.display()),
            Duration::from_secs(20),
            None,
            None,
            false,
        );
        // 清理(忽略失败:目录残留无害)。
        let _ = std::fs::remove_dir_all(&ws);

        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("spike 起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        let text = format!("{}\n{}", out.stdout, out.stderr);
        eprintln!(
            "--- type 工作区内文件 (exit={:?}) ---\n{}",
            out.exit_code, text
        );
        assert!(
            text.contains(marker),
            "ACL 授权未生效:沙箱进程读不到已授权的工作区内文件(命令将完全不可用):\n{text}"
        );
    }

    /// native 命令能在容器内跑、能写工作区文件(cmd 自身 `>` 重定向)。与 appcontainer_sandbox_runs_
    /// native_in_workspace(测 stdout 经伪控制台回传)互补:本测试聚焦"命令确实执行并写盘"。
    #[test]
    fn appcontainer_native_command_runs_and_writes_files() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-ext-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        let out_file = ws.join("out.txt");
        // cmd 用自身的 > 重定向写文件(绕过 console/管道);若文件有内容,说明 native 命令能跑、能产出,
        // 问题只在 console 输出路由;若文件空,说明 native console 程序在容器内根本产不出输出。
        let cmd = format!("cmd /c \"echo NATIVEOK>{}\"", out_file.display());
        let r = run_in_appcontainer_sandbox(&ws, &cmd, Duration::from_secs(20), None, None, false);
        let file_content = std::fs::read_to_string(&out_file).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&ws);
        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        eprintln!(
            "--- native→file (exit={:?}) file=[{}] stdout=[{}] stderr=[{}] ---",
            out.exit_code,
            file_content.trim(),
            out.stdout.trim(),
            out.stderr.trim()
        );
        assert!(
            file_content.contains("NATIVEOK"),
            "AppContainer 内 native 命令连写文件都没产出(console 程序在容器内完全不可用):\nfile=[{}]",
            file_content.trim()
        );
    }

    /// netsh 探测宿主 Windows 防火墙是否在任一 profile 上开启。
    /// AppContainer 的能力放行过滤器由防火墙服务装载;防火墙全关时,能力 SID 已在令牌内也无法出站。
    fn host_firewall_enabled() -> bool {
        std::process::Command::new("C:\\Windows\\System32\\netsh.exe")
            .args(["advfirewall", "show", "allprofiles", "state"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_uppercase().contains(" ON"))
            .unwrap_or(false)
    }

    /// P1:网络隔离证明。
    /// 安全关键、环境无关:**无网络能力 → 出站必被挡**(默认拒绝)。这一条任何环境都断言。
    /// 放行侧(有能力 → 可出站)依赖宿主防火墙开启(WFP 的能力放行过滤器由防火墙服务装载),
    /// 故仅在防火墙开启时断言;关闭时打印说明并跳过(能力是否落入令牌由代码保证,与本断言无关)。
    /// 依赖真实外网(命中 example.com),默认 #[ignore],手动跑:
    ///   cargo test -p mdga-tool-runtime --lib appcontainer_network -- --ignored --nocapture
    #[test]
    #[ignore = "需真实外网;手动用 --ignored 跑"]
    fn appcontainer_network_default_deny_and_capability_gate() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-net-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        // curl 出站探针:成功打印 "RC:200",失败(被挡)则非 200 / 空。
        let probe = "C:\\Windows\\System32\\curl.exe -s -m 8 -o NUL -w \"RC:%{http_code}\" http://example.com";

        let blocked = run_in_appcontainer_sandbox(&ws, probe, Duration::from_secs(20), None, None, false)
            .expect("起进程失败");
        let blocked_text = format!("{}\n{}", blocked.stdout, blocked.stderr);
        eprintln!("--- 无能力出站 (exit={:?}) ---\n{}", blocked.exit_code, blocked_text);

        let allowed = run_in_appcontainer_sandbox(&ws, probe, Duration::from_secs(20), None, None, true)
            .expect("起进程失败");
        let allowed_text = format!("{}\n{}", allowed.stdout, allowed.stderr);
        eprintln!("--- 有能力出站 (exit={:?}) ---\n{}", allowed.exit_code, allowed_text);

        let _ = std::fs::remove_dir_all(&ws);

        // 安全关键断言:默认拒绝必须成立。
        assert!(
            !blocked_text.contains("RC:200"),
            "网络隔离失效:无网络能力却仍能出站(默认应拒绝):\n{blocked_text}"
        );

        // 放行断言:仅在宿主防火墙开启时成立。
        if host_firewall_enabled() {
            assert!(
                allowed_text.contains("RC:200"),
                "网络能力已授但出站被挡(防火墙已开,属真问题):\n{allowed_text}"
            );
        } else {
            eprintln!(
                "[skip] 宿主防火墙全关:跳过「有能力→可出站」断言。\
                 已知 internetClient 能力正确落入令牌;标准机(防火墙开)上方可出站。allowed={allowed_text}"
            );
        }
    }
}
