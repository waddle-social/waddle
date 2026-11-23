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
				set -x
				cat /proc/self/mountinfo | grep -E ' (/|/nix|/nix/store|/tmp|/dev) ' || true
				findmnt -o TARGET,SOURCE,FSTYPE,OPTIONS / /nix /nix/store /tmp /dev 2>&1 || true
				export CUENV_CACHE_DIR="/tmp/cuenv-strict-test"
				rm -rf "$CUENV_CACHE_DIR"
				if command -v strace >/dev/null 2>&1; then
				  strace -f -e trace=unshare,mount,pivot_root,umount2,sethostname,setdomainname cuenv task validateData 2>&1 | grep -E 'EPERM|mount|unshare|pivot_root|sethostname' | tail -n 60 || true
				else
				  python3 - <<'PY'
				import ctypes, os
				libc = ctypes.CDLL("libc.so.6", use_errno=True)
				CLONE_NEWNS=0x00020000; CLONE_NEWUTS=0x04000000; CLONE_NEWIPC=0x08000000; CLONE_NEWUSER=0x10000000; CLONE_NEWPID=0x20000000; CLONE_NEWNET=0x40000000
				MS_RDONLY=1; MS_NOSUID=2; MS_NODEV=4; MS_REMOUNT=32; MS_BIND=4096; MS_REC=16384; MS_PRIVATE=1<<18
				uid, gid = os.getuid(), os.getgid()
				pid = os.fork()
				if pid == 0:
				    try:
				        if libc.unshare(CLONE_NEWUSER|CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWIPC|CLONE_NEWUTS|CLONE_NEWNET) != 0:
				            raise OSError(ctypes.get_errno(), "unshare")
				        print("unshare OK", flush=True)
				        open("/proc/self/setgroups", "wb").write(b"deny")
				        open("/proc/self/uid_map", "wb").write(f"0 {uid} 1\\n".encode())
				        open("/proc/self/gid_map", "wb").write(f"0 {gid} 1\\n".encode())
				        print("id maps OK", flush=True)
				        if libc.sethostname(b"cuenv", 5) != 0:
				            raise OSError(ctypes.get_errno(), "sethostname")
				        if libc.setdomainname(b"", 0) != 0:
				            raise OSError(ctypes.get_errno(), "setdomainname")
				        print("uts OK", flush=True)
				        if libc.mount(None, b"/", None, MS_REC|MS_PRIVATE, None) != 0:
				            raise OSError(ctypes.get_errno(), "mount MS_PRIVATE")
				        print("MS_PRIVATE OK", flush=True)
				        os.makedirs("/tmp/probe-root/nix/store", exist_ok=True)
				        if libc.mount(b"/tmp/probe-root", b"/tmp/probe-root", None, MS_BIND|MS_REC, None) != 0:
				            raise OSError(ctypes.get_errno(), "bind new_root")
				        print("bind new_root OK", flush=True)
				        # Test bind + ro remount of a /nix/store entry and /dev/null
				        sample = next(e.path for e in os.scandir("/nix/store") if e.is_dir())
				        target = f"/tmp/probe-root{sample}".encode()
				        os.makedirs(target, exist_ok=True)
				        if libc.mount(sample.encode(), target, None, MS_BIND, None) != 0:
				            raise OSError(ctypes.get_errno(), f"bind {sample}")
				        print(f"bind {sample} OK", flush=True)
				        if libc.mount(None, target, None, MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV, None) != 0:
				            err = ctypes.get_errno()
				            print(f"remount RO (cuenv flags) FAILED: errno={err} ({os.strerror(err)})", flush=True)
				            st = os.statvfs(target)
				            print(f"statvfs f_flag={hex(st.f_flag)}", flush=True)
				            if libc.mount(None, target, None, MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV|st.f_flag, None) != 0:
				                err2 = ctypes.get_errno()
				                print(f"remount RO (+f_flag) FAILED: errno={err2} ({os.strerror(err2)})", flush=True)
				            else:
				                print("remount RO (+f_flag) SUCCEEDED!", flush=True)
				        else:
				            print("remount RO (cuenv flags) OK", flush=True)
				        open("/tmp/probe-root/devnull", "wb").close()
				        if libc.mount(b"/dev/null", b"/tmp/probe-root/devnull", None, MS_BIND, None) != 0:
				            raise OSError(ctypes.get_errno(), "bind /dev/null")
				        print("bind /dev/null OK", flush=True)
				        if libc.mount(None, b"/tmp/probe-root/devnull", None, MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV, None) != 0:
				            err = ctypes.get_errno()
				            print(f"remount /dev/null FAILED: errno={err} ({os.strerror(err)})", flush=True)
				    except Exception as e:
				        print("FAILED:", e, flush=True)
				    os._exit(0)
				os.waitpid(pid, 0)
				PY
				  cuenv task validateData || true
				fi
				for f in $(find "$CUENV_CACHE_DIR" -path '*/ac/sha256/*/*' -type f 2>/dev/null); do
				  prefix="$(basename "$(dirname "$f")")"
				  rest="$(basename "$f")"
				  h="${prefix}${rest}"
				  echo "=== INSPECT $h ==="
				  cuenv cache inspect "$h" || true
				done
				"""]
			inputs: ["env.cue"]
		}
	}
}
