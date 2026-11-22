// Linux Landlock confines the parser even when it runs under the Server's UID.
use std::os::unix::process::CommandExt;
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    process::Command,
};

const EXEC: u64 = 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const READ: u64 = READ_FILE | READ_DIR;
const FS: u64 = (1 << 15) - 1;
const OUTPUT: u64 = READ | (1 << 1) | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8) | (1 << 14);

#[repr(C, packed)]
struct PathRule {
    access: u64,
    fd: i32,
}

fn check(result: libc::c_long) -> io::Result<libc::c_long> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

fn bpf(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}

pub fn configure(
    command: &mut Command,
    engine: &Path,
    input: &Path,
    output: &Path,
) -> io::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "extractor requires a non-root Server UID",
        ));
    }
    let abi =
        check(unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, 0usize, 0usize, 1u32) })?;
    if abi < 4 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Landlock ABI 4 is required",
        ));
    }
    let attributes = [FS, 3u64]; // Deny TCP bind/connect and filesystem rights except explicit rules.
    let fd = check(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            attributes.as_ptr(),
            size_of_val(&attributes),
            0u32,
        )
    })?;
    let ruleset = unsafe { File::from_raw_fd(fd as i32) };
    for (path, access) in [
        (engine, EXEC | READ_FILE),
        (Path::new("/usr/lib"), EXEC | READ),
        (Path::new("/etc/ld.so.cache"), READ_FILE),
        (input, READ),
        (output, OUTPUT),
    ] {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error)
                if path == Path::new("/etc/ld.so.cache")
                    && error.kind() == io::ErrorKind::NotFound =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        let rule = PathRule {
            access,
            fd: file.as_raw_fd(),
        };
        check(unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                1u32,
                &rule,
                0u32,
            )
        })?;
    }
    let parent = std::process::id();
    // The parser's CLI is single-process. Deny IPC, spawning and metadata
    // mutations which are outside the Landlock ABI 4 filesystem rights.
    let mut filter = vec![
        bpf(0x20, 0, 0, 4),           // seccomp_data.arch
        bpf(0x15, 1, 0, 0xc000_003e), // AUDIT_ARCH_X86_64
        bpf(0x06, 0, 0, 0x8000_0000),
        bpf(0x20, 0, 0, 0),           // seccomp_data.nr
        bpf(0x35, 0, 1, 0x4000_0000), // Reject the alternate x32 syscall table.
        bpf(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32),
    ];
    for syscall in [
        libc::SYS_socket,
        libc::SYS_socketpair,
        libc::SYS_clone,
        libc::SYS_clone3,
        libc::SYS_fork,
        libc::SYS_vfork,
        libc::SYS_io_uring_setup,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_open,
        libc::SYS_pidfd_getfd,
        libc::SYS_kill,
        libc::SYS_tkill,
        libc::SYS_tgkill,
        libc::SYS_chmod,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchmodat2,
        libc::SYS_chown,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_lchown,
        libc::SYS_utime,
        libc::SYS_utimes,
        libc::SYS_utimensat,
        libc::SYS_futimesat,
        libc::SYS_setxattr,
        libc::SYS_lsetxattr,
        libc::SYS_fsetxattr,
        libc::SYS_removexattr,
        libc::SYS_lremovexattr,
        libc::SYS_fremovexattr,
    ] {
        filter.push(bpf(0x15, 0, 1, syscall as u32));
        filter.push(bpf(0x06, 0, 0, 0x0005_0000 | libc::EPERM as u32));
    }
    filter.push(bpf(0x06, 0, 0, 0x7fff_0000));
    unsafe {
        command.pre_exec(move || {
            check(libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0).into())?;
            if libc::getppid() as u32 != parent {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0).into())?;
            for (resource, max) in [
                (libc::RLIMIT_CPU, 600),
                (libc::RLIMIT_AS, 1536 * 1024 * 1024),
                (libc::RLIMIT_FSIZE, 16 * 1024 * 1024 * 1024),
                (libc::RLIMIT_NOFILE, 64),
                (libc::RLIMIT_CORE, 0),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: max,
                    rlim_max: max,
                };
                check(libc::setrlimit(resource, &limit).into())?;
            }
            check(libc::syscall(
                libc::SYS_landlock_restrict_self,
                ruleset.as_raw_fd(),
                0u32,
            ))?;
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_mut_ptr(),
            };
            check(
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &program,
                    0,
                    0,
                )
                .into(),
            )?;
            Ok(())
        });
    }
    Ok(())
}
