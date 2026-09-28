#include <mlibc/elf/startup.h>
#include <stdint.h>
#include <stdlib.h>

extern "C" void __dlapi_enter(uintptr_t *);

extern char **environ;

namespace {

/// The environment a process starts with before anything is put into it.
///
/// mlibc's own `environ` is `char **environ = empty_environment;` -- a
/// *dynamic* initializer, because the initializer is the address of a local
/// anonymous-namespace array rather than a constant. Dynamic initializers live in
/// `.init_array`, and this kernel runs no constructors, so that initializer never
/// happens and `environ` is left as whatever `.bss` holds: null.
///
/// Everything that touches the environment then dereferences it. `getenv` walks
/// `environ[i]`; `setenv` calls `update_vector`, which copies from `environ[i]`.
/// So the first program to read or write any environment variable takes a read
/// fault at address zero.
///
/// This is the same empty vector mlibc's initializer would have installed, and it
/// is a static initializer -- the address of an object -- so it needs no
/// constructor either.
char *initial_environ[] = { nullptr };

} // namespace

extern "C" void __mlibc_entry(uintptr_t *entry_stack, int (*main_fn)(int argc, char *argv[], char *env[])) {
	// Before anything that could want the environment.
	environ = initial_environ;

	// Parse the initial stack here rather than leaving it to `init_libc`.
	//
	// `init_libc` does this job as a constructor, for the same reason `environ`
	// needed seeding: on a system whose loader runs `.init_array` that is the
	// natural place. Here it never runs, so `mlibc::entry_stack` stayed zeroed
	// and `main` was handed argc=0 with argv and envp both null -- quieter than
	// the environ fault and just as wrong. A program that reads its own arguments
	// saw none, and argv[0] is the basis of applet dispatch, so every applet
	// would have presented itself as `busybox`.
	mlibc::exec_stack_data data;
	mlibc::parse_exec_stack(reinterpret_cast<void *>(entry_stack), &data);
	// Publish it where `init_libc` would have, so the rest of mlibc -- the parts
	// that read `entry_stack` rather than main's parameters -- sees the truth.
	mlibc::entry_stack = data;

	// `environ` points straight at the stack's envp.
	//
	// mlibc's `set_startup_data` would instead walk the incoming variables and
	// `putenv` each one, which copies them into mlibc's own vector. That vector is
	// `frg::vector`, so the first `push_back` allocates -- and mlibc's allocator is
	// a `thread_local`, so it reads the thread pointer out of %fs. This kernel does
	// not yet set %fs for a freshly exec'd image, because the loader does not lay
	// out a thread block: mlibc normally has its dynamic loader do that, and here
	// the kernel is the loader instead. So the round trip through `putenv` faults
	// on the first environment variable, before `main` is reached at all.
	//
	// Pointing at the stack's own envp sidesteps that and is what mlibc's own
	// comment there contemplates ("TODO: Copy the arguments instead of pointing to
	// them?"). The envp block lives in the program's address space, below the
	// initial `%rsp`, and stays valid for the life of the process -- which is
	// exactly the lifetime `environ` needs.
	//
	// The consequence is that `environ` is the kernel's array, not a copy: the
	// strings are not private to the process and `environ` itself is not writable.
	// Reading the environment is what this port supports today; `setenv` and
	// `unsetenv` need mlibc's own vector, and therefore need the thread block.
	environ = data.envp;

	__dlapi_enter(entry_stack);

	auto result = main_fn(data.argc, data.argv, data.envp);
	exit(result);
}
