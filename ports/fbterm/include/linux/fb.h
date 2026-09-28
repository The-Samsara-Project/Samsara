/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * <linux/fb.h> for the Samsara fbterm port.
 *
 * fbterm is written against the Linux framebuffer ABI, and that is a reasonable
 * ABI to honour: a small, stable description of a linear pixel buffer, and
 * honouring it is what lets fbterm's rendering code be reused unchanged instead
 * of rewritten.
 *
 * The layouts and request numbers below are Linux's own, copied field for
 * field. That is not stylistic. These structs are filled by the *kernel* -- see
 * FbDevice::ioctl in kernel/src/vfs/devfs.rs -- and the two sides exchange them
 * as a fixed-size block, so a single wrong field width silently shifts
 * everything after it. A `ywrapstep` declared `unsigned long` where Linux has
 * `__u16` puts four bytes of padding in the wrong place, and fbterm would then
 * read a line length out of the middle of a capability field. Nothing would
 * fault; the screen would just be wrong.
 *
 * So: the full structs appear here, in Linux's exact field order, with Linux's
 * exact types -- including the fields this port does not implement. Declaring
 * an unimplemented field is not a promise to support it; the fields that matter
 * are the ones the Samsara side actually fills, and those are documented at the
 * fill site.
 */

#ifndef _LINUX_FB_H
#define _LINUX_FB_H

#include <stdint.h>

/* Request numbers. Linux's encoding, unmodified. The low two bits say how the
 * kernel learns the argument's size; the direction bit separates get from put.
 *
 * Only FBIOGET_VSCREENINFO and FBIOGET_FSCREENINFO are served by Samsara --
 * those are the two fbterm needs to learn the geometry. The rest are declared so
 * that fbdev.cpp compiles unmodified and so that the calls it makes fail
 * honestly rather than being deleted: a program that believes it panned the
 * display when it did not would scroll into nothing. */
#define FBIOGET_FSCREENINFO 0x4602
#define FBIOPUT_FSCREENINFO 0x4603
#define FBIOGET_VSCREENINFO 0x4600
#define FBIOPUT_VSCREENINFO 0x4601
#define FBIOGET_CMAP 0x4604
#define FBIOPUT_CMAP 0x4605
#define FBIOPAN_DISPLAY 0x4606
#define FBIOPUTCMAP 0x4607
#define FBIOGETCMAP 0x4604

/* Pixel storage classes. */
#define FB_TYPE_PACKED_PIXELS 0
#define FB_TYPE_ARGB3328 1
#define FB_TYPE_PACKED_PLANES 2

/* Visual types. */
#define FB_VISUAL_PSEUDOCOLOR 1
#define FB_VISUAL_TRUECOLOR 2
#define FB_VISUAL_DIRECTCOLOR 3
#define FB_VISUAL_PSEUDODIRECTCOLOR 4

/* Vertical panning bits in fb_var_screeninfo::vmode. */
#define FB_VMODE_YWRAP 0x001
#define FB_VMODE_PAN 0x002

/*
 * One colour channel: where it starts in the pixel, how wide it is, and whether
 * its most significant bit is rightmost.
 *
 * `msb_right` is declared but never set by the Samsara fill path. On x86 the
 * framebuffer is always little-endian and the channels are left-aligned within
 * the pixel, so the correct value is 0 -- which is also the struct's zero
 * initialisation, so leaving it alone is correct rather than merely convenient.
 */
struct fb_bitfield {
	uint32_t offset;	/* bit position within the pixel */
	uint32_t length;	/* number of bits in the channel */
	uint32_t msb_right;	/* != 0: most significant bit is rightmost */
};

/*
 * The display mode.
 *
 * Field order and types are Linux's. The members Samsara populates are xres,
 * yres, xres_virtual, yres_virtual, bits_per_pixel, vmode, and the three
 * colour bitfields; everything else is left zeroed, which is the honest value
 * for a display with no pan range, no physical dimensions and no non-standard
 * timing.
 */
struct fb_var_screeninfo {
	uint32_t xres;
	uint32_t yres;
	uint32_t xres_virtual;
	uint32_t yres_virtual;
	uint32_t xoffset;
	uint32_t yoffset;
	uint32_t bits_per_pixel;
	uint32_t grayscale;
	struct fb_bitfield red;
	struct fb_bitfield green;
	struct fb_bitfield blue;
	struct fb_bitfield transp;
	uint32_t nonstd;
	uint32_t activate;
	uint32_t height;		/* physical height in mm; 0 = unknown */
	uint32_t width;		/* physical width in mm; 0 = unknown */
	uint32_t accel_flags;
	uint32_t pixclock;		/* pixel clock in picoseconds */
	uint32_t left_margin;
	uint32_t right_margin;
	uint32_t upper_margin;
	uint32_t lower_margin;
	uint32_t hsync_len;
	uint32_t vsync_len;
	uint32_t sync;
	uint32_t vmode;
	uint32_t rotate;
	uint32_t colorspace;
	uint32_t reserved[4];
};

/*
 * The display's fixed properties.
 *
 * The two that decide behaviour rather than describe geometry are the pan steps,
 * and the Samsara side sets both to zero. That is not a stub: it is the correct
 * answer. The bootloader's framebuffer tag describes a single visible window
 * with no larger virtual screen behind it, so the hardware cannot pan, and a
 * non-zero step here would make fbterm believe it could scroll by shifting a
 * window, and would then scroll into whatever happens to be mapped next.
 */
struct fb_fix_screeninfo {
	char id[16];			/* driver id; not populated, so zeroed */
	/*
	 * Named smem_len, and it holds a LENGTH in bytes -- not a physical
	 * address.
	 *
	 * This is the historical Linux spelling, in the position modern Linux
	 * gives to `smem_start`. Keeping the old name is not nostalgia: fbdev.cpp
	 * reads this member as the length argument to mmap()
	 * (`mmap(0, finfo.smem_len, ...)`), and the Samsara fill path therefore
	 * stores the size of the mappable range here. Renaming the member to
	 * `smem_start` without changing that would be a memory-corrupting bug
	 * wearing a correct-looking header: fbterm would pass a physical address
	 * as a length and try to map a terabyte.
	 */
	unsigned long smem_len;		/* length of the mappable range, bytes */
	uint32_t type;			/* FB_TYPE_* */
	uint32_t type_aux;
	uint32_t visual;		/* FB_VISUAL_* */
	uint16_t xpanstep;		/* 0: cannot pan horizontally */
	uint16_t ypanstep;		/* 0: cannot pan vertically */
	uint16_t ywrapstep;		/* 0: cannot wrap vertically */
	unsigned long line_length;	/* bytes per scanline */
	unsigned long mmio_start;	/* not an MMIO device; 0 */
	uint32_t mmio_len;
	uint32_t accel;
	uint16_t capabilities;
	uint16_t reserved[2];
};

/*
 * A colour ramp, for pseudo-colour and direct-colour displays.
 *
 * Declared because fbdev.cpp references it in setupPalette(). On Samsara the
 * display is FB_VISUAL_TRUECOLOR, and setupPalette() returns immediately in
 * that case, so this is never filled. The pointers are left null for the same
 * reason: a non-null pointer to a zeroed struct would be worse than absent.
 */
struct fb_cmap {
	uint32_t start;
	/*
	 * fbterm's name for the entry count. Linux spells this `end`; the port
	 * keeps `len` because that is the member fbdev.cpp's INIT_CMAP macro
	 * assigns, and a struct member cannot be renamed on the caller's side.
	 * It is the number of entries, inclusive of `start` -- not an exclusive
	 * end index, which is the other way this field is usually written and is
	 * worth being unambiguous about.
	 */
	uint32_t len;
	uint16_t *red;
	uint16_t *green;
	uint16_t *blue;
	uint8_t *transp;
};

#endif /* _LINUX_FB_H */
