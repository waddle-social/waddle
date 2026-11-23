package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

// Waddle for Linux (GTK4 + libadwaita). Its own Cargo workspace and its own
// dev shell (`devShells.<system>.linux`), so GTK never enters the server's
// builds. See claude/linux-client-plan.md.
let _rustInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"rust-toolchain.toml",
	"src/**",
	"data/**",
	schema.#ModuleFiles & {paths: [
		"flake.nix",
		"flake.lock",
		// The shared core and the protocol facade, by path.
		"server/Cargo.toml",
		"server/crates/waddle-client-session/**",
		"server/crates/waddle-xmpp-client-ffi/**",
		"server/crates/waddle-xmpp-client/**",
		"server/crates/waddle-xmpp-core/**",
	]},
]

schema.#Project & {
	name: "waddle-linux"

	runtime: {
		type:   "nix"
		flake:  "../.."
		output: "devShells.x86_64-linux.linux"
	}

	cache: remote: wc.#NamespaceRemoteCache

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		wc.#NamespaceBazelCache,
		wc.#CuenvRelease,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64": "namespace-profile-linux-x86"
			amd64:       "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
		default: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.fmt, tasks.clippy, tasks.test, tasks.validateData]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.checkCiDrift, tasks.fmt, tasks.clippy, tasks.test, tasks.validateData, tasks.debugCache]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", schema.#ModuleFiles & {paths: [".github/workflows/waddle-linux-default.yml", ".github/workflows/waddle-linux-pullrequest.yml"]}]
		}

		fmt: schema.#Task & {
			command: "cargo"
			args: ["fmt", "--check"]
			inputs: _rustInputs
		}

		// Same posture as the server: warnings are errors.
		clippy: schema.#Task & {
			command: "cargo"
			args: ["clippy", "--locked", "--all-targets", "--", "-D", "warnings"]
			inputs: _rustInputs
		}

		test: schema.#Task & {
			command: "cargo"
			args: ["test", "--locked"]
			inputs: _rustInputs
		}

		// The desktop entry and AppStream metadata Flathub and software
		// centres read.
		validateData: schema.#Task & {
			command: "bash"
			args: ["-c", "desktop-file-validate data/social.waddle.Waddle.desktop && appstreamcli validate --no-net data/social.waddle.Waddle.metainfo.xml"]
			cache: mode: "read-write"
			hermetic: sandbox: "auto"
			inputs: ["data/**"]
		}

		debugCache: schema.#Task & {
			hermetic: false
			command:  "bash"
			args: ["-c", """
				grep -E '^(Seccomp|NoNewPrivs|Cap)' /proc/self/status || true
				python3 - <<'PY'
				import ctypes, os, tempfile
				libc = ctypes.CDLL("libc.so.6", use_errno=True)
				CLONE_NEWNS=0x00020000; CLONE_NEWUTS=0x04000000; CLONE_NEWIPC=0x08000000; CLONE_NEWUSER=0x10000000; CLONE_NEWPID=0x20000000; CLONE_NEWNET=0x40000000
				MS_RDONLY=1; MS_NOSUID=2; MS_NODEV=4; MS_REMOUNT=32; MS_BIND=4096; MS_REC=16384; MS_PRIVATE=1<<18
				MNT_DETACH=2; SYS_pivot_root=155; SYS_capset=126; SYS_close_range=436; SYS_pidfd_open=434
				PR_SET_SECUREBITS=28; PR_SET_NO_NEW_PRIVS=38; PR_SET_DUMPABLE=4; PR_SET_PDEATHSIG=1
				O_PATH=0o10000000; O_NOFOLLOW=0o400000; O_CLOEXEC=0o2000000
				uid, gid = os.getuid(), os.getgid()
				d = tempfile.mkdtemp(prefix="probe-")
				new_root = os.path.join(d, "root")
				put_old = os.path.join(new_root, ".old_root")
				os.makedirs(put_old, exist_ok=True)
				pid = os.fork()
				if pid == 0:
				    def check(label, rc):
				        err = ctypes.get_errno() if rc < 0 else 0
				        print(f"{label}: rc={rc} errno={err} ({os.strerror(err) if err else 'OK'})", flush=True)
				    check("setpgid", libc.setpgid(0, 0))
				    check("PR_SET_PDEATHSIG", libc.prctl(PR_SET_PDEATHSIG, 9, 0, 0, 0))
				    check("unshare", libc.unshare(CLONE_NEWUSER|CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWIPC|CLONE_NEWUTS|CLONE_NEWNET))
				    open("/proc/self/setgroups", "wb").write(b"deny")
				    open("/proc/self/uid_map", "wb").write(f"0 {uid} 1\\n".encode())
				    open("/proc/self/gid_map", "wb").write(f"0 {gid} 1\\n".encode())
				    check("sethostname", libc.sethostname(b"cuenv", 5))
				    check("setdomainname", libc.setdomainname(b"", 0))
				    check("MS_PRIVATE", libc.mount(None, b"/", None, MS_REC|MS_PRIVATE, None))
				    check("bind new_root", libc.mount(new_root.encode(), new_root.encode(), None, MS_BIND|MS_REC, None))
				    # Test O_PATH bind of /nix/store entry AND host ROOTS (/usr, /etc/ssl, /nix/store)
				    for src in ["/usr", "/bin", "/lib", "/lib64", "/etc/ssl", "/nix/store"]:
				        if not os.path.exists(src) or os.path.islink(src):
				            continue
				        tgt = os.path.join(new_root, src.lstrip("/"))
				        os.makedirs(tgt, exist_ok=True)
				        rc = libc.mount(src.encode(), tgt.encode(), None, MS_BIND, None)
				        if rc != 0:
				            check(f"bind {src}", rc)
				        else:
				            rc2 = libc.mount(None, tgt.encode(), None, MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV, None)
				            if rc2 != 0:
				                check(f"remount RO {src}", rc2)
				    sample = next(e.path for e in os.scandir("/nix/store") if e.is_dir())
				    fd = os.open(sample, O_PATH|O_NOFOLLOW|O_CLOEXEC)
				    stgt = os.path.join(new_root, sample.lstrip("/"))
				    os.makedirs(stgt, exist_ok=True)
				    check(f"O_PATH bind {sample}", libc.mount(f"/proc/self/fd/{fd}".encode(), stgt.encode(), None, MS_BIND, None))
				    os.close(fd)
				    check("O_PATH remount RO", libc.mount(None, stgt.encode(), None, MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV, None))
				    check("pivot_root", libc.syscall(SYS_pivot_root, new_root.encode(), put_old.encode()))
				    os.chdir("/")
				    check("umount2 .old_root", libc.umount2(b"/.old_root", MNT_DETACH))
				    check("PR_SET_SECUREBITS", libc.prctl(PR_SET_SECUREBITS, 3, 0, 0, 0))
				    check("PR_SET_NO_NEW_PRIVS", libc.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
				    class CapHdr(ctypes.Structure): _fields_ = [("version", ctypes.c_uint32), ("pid", ctypes.c_int32)]
				    class CapData(ctypes.Structure): _fields_ = [("e", ctypes.c_uint32), ("p", ctypes.c_uint32), ("i", ctypes.c_uint32)]
				    hdr = CapHdr(0x20080522, 0)
				    data = (CapData * 2)()
				    check("capset", libc.syscall(SYS_capset, ctypes.byref(hdr), ctypes.byref(data)))
				    check("PR_SET_DUMPABLE", libc.prctl(PR_SET_DUMPABLE, 0, 0, 0, 0))
				    check("close_range", libc.syscall(SYS_close_range, 3, 0xffffffff, 4))
				    pfd = libc.syscall(SYS_pidfd_open, os.getpid(), 0)
				    check("pidfd_open", pfd)
				    os._exit(0)
				os.waitpid(pid, 0)
				PY
				"""]
			inputs: ["env.cue"]
		}
	}
}
