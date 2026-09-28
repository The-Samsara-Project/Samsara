/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <getopt.h> for the Samsara fbterm port.
 *
 * mlibc provides POSIX getopt(3) -- the function and all four globals
 * (optarg, optind, opterr, optopt) -- but not the GNU getopt_long(3) or the
 * <getopt.h> header that declares it. fbterm parses its command line with
 * getopt_long, so the port supplies both.
 *
 * getopt_long is a strict superset of getopt: the short-option behaviour is
 * already correct in the libc and is not reimplemented here. What is added is
 * long-option matching, and the hard part of that -- permuting argv so options
 * may follow operands -- is POSIX getopt's own job, which the libc already
 * does.
 *
 * The state this shares with the libc's getopt (optind in particular) is
 * exactly the state getopt_long is specified to share, so a program that mixes
 * the two calls sees one consistent parse rather than two interleaved ones.
 */

#ifndef _GETOPT_H
#define _GETOPT_H

#ifdef __cplusplus
extern "C" {
#endif

/* Values for an entry in the long-option table, matching the values the C
 * library's getopt returns so a caller can switch on them uniformly. */
#define no_argument 0
#define required_argument 1
#define optional_argument 2

/*
 * One long option.
 *
 * The layout is GNU's exactly -- name, has_arg, flag, val -- and it has to be.
 * Callers initialise their tables positionally, as fbterm does with
 * `{ "font-names", required_argument, 0, 'n' }`, so a reordered struct does not
 * fail to compile: it compiles, and then puts the short character into `flag`,
 * which is never dereferenced, and returns 0 for every option on the command
 * line. The program then runs with none of its settings applied and no error
 * anywhere. Matching glibc field for field is the only safe choice.
 */
struct option {
	const char *name;
	int has_arg;
	int *flag;
	int val;
};

extern char *optarg;
extern int optind, opterr, optopt;

/*
 * GNU getopt_long(3).
 *
 * Behaves as POSIX getopt for short options, and additionally matches a long
 * option against the table in `longopts`. When an entry has a non-NULL `val`
 * that field is set and 0 is returned, which is GNU's way of saying "handled,
 * but there is no short character to return"; with `val` NULL the entry's
 * `flag` is consulted the same way.
 */
int getopt_long(int argc, char *const *argv, const char *shortopts,
                const struct option *longopts, int *longindex);

#ifdef __cplusplus
}
#endif

#endif /* _GETOPT_H */
