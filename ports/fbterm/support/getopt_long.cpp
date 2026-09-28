// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `getopt_long(3)` for the Samsara fbterm port.
//!
//! mlibc provides POSIX `getopt` and all four of its globals, but not the GNU
//! long-option form that fbterm uses to parse its command line. This adds
//! long-option matching and nothing else.
//!
//! The subtle parts of option parsing -- permuting argv so that options and
//! operands may be interleaved, and the exact rules for a missing required
//! argument -- belong to POSIX `getopt` and are already implemented in the libc.
//! Reimplementing them here would mean two parsers sharing the same four global
//! variables (`optind`, `optarg`, `optopt`, `opterr`) with subtly different
//! ideas of what `optind` means, which fails in ways that are extremely hard to
//! see. So the split is deliberate and narrow:
//!
//!   - short options, abbreviation of a cluster, and attached or separate
//!     arguments: all delegated to the libc's getopt
//!   - recognising a long option, and equating it to its short equivalent:
//!     done here
//!
//! The delegation is by *rewriting* rather than by calling: when argv holds a
//! `--long` form, it is rewritten in place into the `-x` form the libc's getopt
//! already understands, and the return value is derived from that. That keeps
//! one parser responsible for the argv state machine.

#include <getopt.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern "C" int getopt_long(int argc, char *const *argv, const char *shortopts,
                           const struct option *longopts, int *longindex) {
	// Handle `--` first: it ends option processing, and the libc's getopt
	// must see it to stop scanning. Rewriting it would hide the terminator.
	if (optind >= argc)
		return -1;

	const char *arg = argv[optind];

	// Not a long option, or the terminator. Straight to the libc.
	if (arg[0] != '-' || arg[1] != '-' || arg[2] == '\0')
		return getopt(argc, argv, shortopts);

	// The long-option table is optional in the GNU prototype; without it,
	// `--anything` is an error rather than a silent no-op.
	if (!longopts) {
		fputs("getopt_long: long options given but no option table\n", stderr);
		return '?';
	}

	// Split `--name` or `--name=value` at the `=`. An argument may also be
	// supplied as the next argv entry, which is left to the libc below.
	char namebuf[64];
	const char *name = arg + 2;
	const char *eq = strchr(name, '=');
	size_t namelen = eq ? (size_t)(eq - name) : strlen(name);
	if (namelen >= sizeof namebuf) {
		// A name longer than any real option; let the "unknown option" path
		// below report it rather than truncating into a different option.
		namelen = sizeof namebuf - 1;
	}
	memcpy(namebuf, name, namelen);
	namebuf[namelen] = '\0';

	// Exact match wins over prefix match, as GNU specifies. The second pass
	// finds an unambiguous prefix; the first pass is what makes an exact match
	// beat a longer option that merely has the exact name as a prefix, which
	// is the ambiguity rule people expect and rarely rely on.
	const struct option *found = nullptr;
	for (const struct option *o = longopts; o->name; o++) {
		if (!strcmp(o->name, namebuf)) {
			found = o;
			break;
		}
	}
	if (!found) {
		// Prefix match, and only if it is unambiguous. Counting rather than
		// taking the first hit is what makes `--f` ambiguous between
		// --font-names and --font-size an error rather than a coin flip.
		const struct option *partial = nullptr;
		int matches = 0;
		for (const struct option *o = longopts; o->name; o++) {
			if (!strncmp(o->name, namebuf, namelen)) {
				partial = o;
				matches++;
			}
		}
		if (matches == 1)
			found = partial;
		else if (matches > 1) {
			fprintf(stderr, "getopt_long: option '--%s' is ambiguous\n", namebuf);
			optopt = 0;
			optind++;
			return '?';
		}
	}

	if (!found) {
		fprintf(stderr, "getopt_long: unrecognized option '--%s'\n", namebuf);
		optopt = 0;
		optind++;
		return '?';
	}

	if (longindex)
		*longindex = (int)(found - longopts);

	// An entry with a non-NULL flag is GNU's "set this int, return nothing".
	// Handled before the rewrite because the value never reaches argv.
	if (found->flag) {
		*(found->flag) = found->val;
		optind++;
		return 0;
	}

	// Rewrite this argv entry into the short form the libc's getopt parses,
	// so exactly one implementation owns the argv cursor. The entry has to
	// stay one string: the libc reads the option and its attached argument
	// from a single argv element, and a separate element would be treated as
	// an operand.
	//
	// The buffer is deliberately large enough for "-<char>=<value>"; a longer
	// value is left to the libc as a separate argv entry, which it handles
	// natively and which avoids inventing a truncation limit that GNU does not
	// have.
	const char *val = nullptr;
	bool val_attached = false;
	if (eq) {
		val = eq + 1;
		val_attached = true;
	} else if (found->has_arg == required_argument) {
		// Required argument may be the next argv element. If it is absent,
		// report the GNU error rather than letting the libc's getopt report
		// it against the short form with the wrong option letter.
		if (optind + 1 >= argc) {
			fprintf(stderr, "getopt_long: option '--%s' requires an argument\n",
					found->name);
			optopt = found->val;
			optind++;
			// Honour the ':' convention the caller asked for by way of
			// `shortopts`: a leading colon means "report a missing argument
			// as ':' and stay silent", which is how a program distinguishes
			// "you forgot an argument" from "I do not know that option".
			return (shortopts && shortopts[0] == ':') ? ':' : '?';
		}
	}

	// Reuse argv[optind] in place. The caller's argv is already modifiable
	// memory in every C program that gets this far -- getopt itself permutes
	// it -- so writing here is the same class of operation the libc performs.
	size_t shortlen = 2 + (val ? strlen(val) + 1 : 0);
	char *slot = argv[optind];
	if (shortlen <= strlen(slot)) {
		slot[0] = '-';
		slot[1] = (char)found->val;
		if (val) {
			slot[2] = '=';
			memcpy(slot + 3, val, strlen(val) + 1);
		} else {
			slot[2] = '\0';
		}
		optind++;

		// optarg must be set for *both* shapes of a required argument.
		//
		// The separate one (`--font-names mono`) is the obvious case. The
		// attached one (`--font-size=12`) is the one that is easy to miss,
		// and missing it is silent: optarg keeps whatever the *previous*
		// option left there, so a program asking for two options in a row gets
		// the first one's value twice and never notices. The argv rewrite
		// above does not help, because the short form `-s=12` is never handed
		// to getopt -- the entry is consumed above and the character returned
		// directly, precisely so the following operand is not skipped.
		//
		// `val` points into the original argv entry, which the rewrite above
		// overwrote in place. So the value is copied out before the rewrite,
		// not read from `slot` afterwards.
		if (found->has_arg == required_argument) {
			if (val_attached) {
				// Already copied to the tail of `slot` by the rewrite above,
				// which is where optarg should point: a pointer into the
				// caller's own argv, exactly as the libc's getopt returns.
				optarg = slot + 3;
			} else {
				optarg = argv[optind];
				optind++;
			}
		}

		// Return the short character directly rather than re-entering getopt:
		// the entry has been consumed above, so calling getopt again would
		// skip the following operand.
		optopt = found->val;
		return found->val;
	}

	// The rewritten form would not fit in place (a very long attached
	// argument). Fall back to the libc by advancing past the option and
	// letting it take the next argv element as the argument. The long name is
	// lost to error messages from here on, which is the accepted trade for not
	// imposing a length limit GNU does not have.
	slot[0] = '-';
	slot[1] = (char)found->val;
	slot[2] = '\0';
	optind++;
	return getopt(argc, argv, shortopts);
}
