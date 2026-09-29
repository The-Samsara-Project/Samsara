#include <mlibc/elf/startup.h>
#include <mlibc/stack_protector.hpp>
#include <mlibc/tcb.hpp>
#include <mlibc/thread.hpp>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

extern "C" void __dlapi_enter(uintptr_t *);

extern char **environ;

/// The stack canary the compiled-in code compares against.
///
/// mlibc defines it in `options/internal/gcc/stack_protector.cpp` but declares it
/// nowhere public, so this is the same extern mlibc's own dynamic loader
/// declares in `linker.cpp` before reading it.
extern "C" uintptr_t __stack_chk_guard;

/// Auxv tag carrying the address of this image's thread-control block.
///
/// A Samsara extension; `AT_TCB` in `kernel/src/elf.rs` is the other half of it
/// and the two have to agree. Linux has no tag for this because on Linux the
/// dynamic loader builds the block and so never has to be told where it is --
/// there is no case where the kernel knows and the libc does not. That case is
/// this one, and the number sits above Linux's whole auxv range so it cannot
/// collide with a tag added upstream later.
constexpr uintptr_t AT_TCB = 0x100;

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

/// The stack layout the kernel builds, in the order it builds it: `argc`, the
/// argument pointers and their NULL, the environment pointers and their NULL,
/// then the auxv pairs up to `AT_NULL`.
struct InitialStack {
	int argc;
	uintptr_t *argv;
	uintptr_t *envp;
	const uintptr_t *auxv;
};

/// Split the initial stack into its three parts.
///
/// mlibc has `parse_exec_stack`, which reads `argc`/`argv`/`envp` and stops at
/// the envp terminator. It does not expose the auxv that follows, and that is
/// where the thread-control block's address is, so this walks the same three
/// steps and then hands back a pointer to the auxv rather than reparsing
/// anything.
InitialStack split_initial_stack(uintptr_t *sp) {
	InitialStack s;
	s.argc = static_cast<int>(sp[0]);
	s.argv = &sp[1];
	s.argv += s.argc;
	// The NULL after argv, then the environment block, then the NULL after it.
	// `envp` has to point at the *first* string, not the terminator, because that
	// is the address a program walking the environment expects to start from.
	s.envp = s.argv + 1;
	uintptr_t *after_env = s.envp;
	while (*after_env)
		++after_env;
	s.auxv = reinterpret_cast<const uintptr_t *>(after_env + 1);
	return s;
}

/// The value of auxv tag `tag`, or 0 if the image's stack does not carry it.
uintptr_t auxv_value(const uintptr_t *auxv, uintptr_t tag) {
	for (; auxv[0] != 0; auxv += 2) {
		if (auxv[0] == tag)
			return auxv[1];
	}
	return 0;
}

/// The thread-control block, built in the address the loader staged for it.
///
/// `mlibc::get_current_tcb` is a bare `mov %fs:0`, so the address the kernel put
/// in the thread pointer *is* where this object has to be -- there is nowhere
/// else to put it and no way to move it afterwards.
///
/// Everything here mirrors what mlibc's own dynamic loader does in
/// `allocateTcb`, minus the parts that only exist to serve dynamically loaded
/// objects. The split of labour is worth stating plainly, because it is the
/// whole reason this file is not three lines:
///
///   * The *kernel* lays out the block. It is the loader here -- mlibc's
///     dynamic loader is not built for this port, and the kernel is what
///     replaced it -- so it is what parses `PT_TLS`, maps the pages, copies the
///     initialized thread-local data, and installs the thread pointer. It cannot
///     construct this object, because `Tcb` is a C++ type in a library the
///     kernel does not link.
///
///   * This file *constructs* the object. Only compiled mlibc knows the type.
///
/// The layout the kernel used is the one the toolchain assumed: thread-local
/// data at the bottom, the thread-control block immediately above it, and the
/// thread pointer at the top. `sizeof(Tcb)` has to fit in the space reserved for
/// it, which is what the assertion below is for.
auto build_thread_block(uintptr_t *entry_stack) -> Tcb * {
	// The kernel reserved this much for the thread-control block. It cannot
	// measure `Tcb` itself, so it reserves a rounded-up block and relies on this
	// assertion to keep that honest: if mlibc's `Tcb` ever outgrew the
	// reservation, the port would stop compiling rather than the kernel writing
	// a thread-control block past the end of its own memory. The two numbers
	// have to be changed together -- `TCB_RESERVE` in `kernel/src/elf.rs`.
	static_assert(sizeof(Tcb) <= 256, "Tcb does not fit the loader's TCB_RESERVE");

	// The address comes from the auxv rather than from the thread pointer, which
	// is the whole point of `AT_TCB`. mlibc finds the current thread block by
	// reading the thread pointer and treating what it finds there as the block's
	// address -- but the first field *inside* the block is a self-pointer, so
	// that read returns 0 until something has already written the address in.
	// mlibc's dynamic loader is what normally breaks the cycle, by allocating the
	// block and therefore knowing where it is. There is no dynamic loader here;
	// the kernel is the loader, so the kernel stages the address and this is
	// where it is read.
	//
	// 0 means the image had no thread-local storage at all, so there is no block.
	// That is a real case -- a freestanding program linked without a thread-local
	// variable -- and it is not an error, so this returns rather than aborting.
	uintptr_t addr = auxv_value(split_initial_stack(entry_stack).auxv, AT_TCB);
	if (!addr)
		return nullptr;

	// `get_current_tcb` would return the self-pointer, which is not set yet. The
	// thread pointer itself is the block's address, so the staged address *is* the
	// block. They are the same number by construction -- see `map_thread_block` --
	// and writing the self-pointer is what makes the next read agree.
	auto *tcb = reinterpret_cast<Tcb *>(addr);

	// The kernel zeroed the block, so every field starts empty. Only the ones
	// that mean something at startup are set below.
	tcb->selfPointer = tcb;

	// The stack canary, at `fs:0x28` -- the offset GCC's stack protector reads.
	// It has to be the *same* value as `__stack_chk_guard`, which is the one the
	// compiled-in code compares against, and both are seeded here so they cannot
	// disagree. `initStackGuard(nullptr)` picks a fixed value rather than
	// entropy: the kernel's initial stack has no `AT_RANDOM` to draw from yet,
	// so this makes the two agree rather than making either unpredictable.
	// A fixed canary is not protection, but it is correct -- and a canary that
	// disagreed with itself would abort on the first function that has one.
	mlibc::initStackGuard(nullptr);
	tcb->stackCanary = __stack_chk_guard;

	// Cancellation is enabled and never triggered: there are no threads to
	// cancel, so the bit only has to be set the way `allocateTcb` sets it.
	// `tcbCancelEnableBit` is 1, and lives in an anonymous namespace in
	// tcb.hpp, so it is spelled out here.
	tcb->cancelBits = 1;
	tcb->didExit = 0;
	tcb->isJoinable = 1;
	memset(&tcb->returnValue, 0, sizeof(tcb->returnValue));
	tcb->cxaThreadExitHandlers = nullptr;

	// The thread id, from the one source that is authoritative before any libc
	// state exists. `tcb_available_flag` is what tells mlibc's `this_tid` it may
	// read this field at all; left false it would keep reporting a hardcoded 1
	// for every process in the system.
	tcb->tid = getpid();
	mlibc::tcb_available_flag = true;

	// The per-thread key table: 1024 slots of {value, generation}, zeroed by the
	// kernel's fresh pages. `pthread_getspecific` and `pthread_setspecific` index
	// straight into it, so a null pointer here turns "thread-local keys are not
	// supported yet" into a segmentation fault. 16 KiB that nothing else uses is
	// a cheap price for those two calls answering correctly.
	//
	// `mmap` rather than `malloc`, because `malloc`'s pool is itself a
	// thread-local: it is what this whole function exists to make usable.
	void *keys = mmap(nullptr, sizeof(Tcb::LocalKey) * PTHREAD_KEYS_MAX,
	                  PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (keys == MAP_FAILED) {
		// Not fatal, and deliberately not an abort. A process that never calls
		// `pthread_getspecific` does not care, and refusing to start at all
		// would turn a missing feature into a dead machine. The null pointer is
		// the same state the block had before, so the failure is a null deref in
		// a pthread key call rather than anything worse.
		keys = nullptr;
	}
	tcb->localKeys = static_cast<frg::array<Tcb::LocalKey, PTHREAD_KEYS_MAX> *>(keys);

	// The dynamic thread vector stays empty, and that is not an oversight.
	// `dtvPointers` is the per-module thread-local pointer table, and the only
	// code in mlibc that reads it lives in the dynamic loader's `accessDtvIndex`
	// -- which is not built for this port. A statically linked image addresses
	// its thread-local variables as local-exec, a direct `mov %fs:offset` the
	// linker already resolved, so there is nothing to look up and nothing that
	// will ever look. There is no `__tls_get_addr` in these images to ask.
	tcb->dtvSize = 0;
	tcb->dtvPointers = nullptr;

	return tcb;
}

extern "C" void __mlibc_entry(uintptr_t *entry_stack, int (*main_fn)(int argc, char *argv[], char *env[])) {
	// First, before anything that can touch a thread-local variable.
	//
	// `errno` is one, mlibc's allocator's pool is another, and both are reached
	// by ordinary error paths: `errno = e` after a failed `ioctl`, the first
	// `malloc` of anything at all. They read the thread pointer out of `%fs`,
	// which is a `thread_local` access, and without a thread block that is a read
	// of address zero. The symptom is not a clean error -- it is a fault inside
	// a call the program had no reason to expect could fail -- which is how this
	// went unnoticed for so long: fbterm's shell child died on the `errno` write
	// that follows an `ioctl` it made precisely to find out the terminal was not
	// there, and every `malloc`ing program died on its first allocation.
	//
	// The kernel has already pointed `%fs` at a mapped, zeroed block by the time
	// this runs, so this function only has to fill it in.
	build_thread_block(entry_stack);

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
	// a `thread_local`, so it reads the thread pointer out of %fs. The thread block
	// exists by now, so that allocation would actually succeed; it is the copying
	// that is still pointless, because the result would be a duplicate of an array
	// the kernel already built and will outlive the process.
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
	// `unsetenv` need mlibc's own vector, and so need a private copy of the
	// variables this is aliasing.
	environ = data.envp;

	__dlapi_enter(entry_stack);

	auto result = main_fn(data.argc, data.argv, data.envp);
	exit(result);
}
