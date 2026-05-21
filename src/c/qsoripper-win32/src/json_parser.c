/* json_parser.c — Minimal JSON value extractor for QsoRipper Win32
 *
 * Extracted from main.c and hardened:
 *   - NULL checks on all public entry points
 *   - Full JSON whitespace handling (space, tab, CR, LF) after ':'
 *   - Safe numeric parsing via strtol/strtod (no undefined behavior on overflow)
 *   - String-aware brace/bracket matching (braces inside quoted strings are
 *     correctly ignored by extract_object, array_nth)
 *   - Dynamic pattern buffer for keys longer than 126 characters
 *   - Zero-allocation numeric parsing (get_int/get_double parse in-place)
 *   - Top-level-only key lookup: json_get_string/int/double match the named
 *     key only at depth 1 of the outer object, skipping over nested objects,
 *     arrays, and string-value bytes. This prevents string-array elements or
 *     nested-object keys from masquerading as top-level keys.
 */

#define _CRT_SECURE_NO_WARNINGS
#include "json_parser.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <errno.h>
#include <limits.h>

static int is_json_ws(char c)
{
    return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

/* Skip a JSON string starting at *p (which must point to the opening '"').
   Returns a pointer just past the closing '"', or to the terminating NUL if
   the string is unterminated. */
static const char *skip_string(const char *p)
{
    if (*p != '"') return p;
    p++;
    while (*p && *p != '"') {
        if (*p == '\\' && *(p + 1)) p++;
        p++;
    }
    if (*p == '"') p++;
    return p;
}

/* Skip a JSON container (object or array) starting at *p (which must point to
   '{' or '['). Returns a pointer just past the matching closer, or to the
   terminating NUL if unbalanced. String contents are scanned for escapes so
   braces and brackets inside strings are ignored. */
static const char *skip_container(const char *p)
{
    char open = *p;
    if (open != '{' && open != '[') return p;
    char close = (open == '{') ? '}' : ']';
    int depth = 0;
    int in_str = 0;
    int esc = 0;
    for (; *p; p++) {
        if (in_str) {
            if (esc) { esc = 0; continue; }
            if (*p == '\\') { esc = 1; continue; }
            if (*p == '"') { in_str = 0; }
            continue;
        }
        if (*p == '"') { in_str = 1; continue; }
        if (*p == open) depth++;
        else if (*p == close) {
            depth--;
            if (depth == 0) { p++; break; }
        }
    }
    return p;
}

/* Skip a bare scalar (number, true, false, null) until a value terminator. */
static const char *skip_scalar(const char *p)
{
    while (*p && *p != ',' && *p != '}' && *p != ']') p++;
    return p;
}

/* Find the first top-level key matching `key` inside the outer object of
   `json` and return a pointer to the first byte of its value (past the
   colon and any whitespace). Returns NULL if not found.
   Only keys at depth 1 are considered: keys nested inside sub-objects or
   array elements are skipped over, and string-value bytes are never
   interpreted as key text. */
static const char *find_value_start(const char *json, const char *key)
{
    if (!json || !key) return NULL;

    size_t key_len = strlen(key);
    size_t pat_len = key_len + 2;
    char stack_buf[128];
    char *pattern;
    if (pat_len < sizeof(stack_buf)) {
        pattern = stack_buf;
    } else {
        pattern = (char *)malloc(pat_len + 1);
        if (!pattern) return NULL;
    }
    pattern[0] = '"';
    memcpy(pattern + 1, key, key_len);
    pattern[1 + key_len] = '"';
    pattern[2 + key_len] = '\0';

    const char *p = json;
    while (*p && is_json_ws(*p)) p++;
    if (*p != '{') {
        if (pattern != stack_buf) free(pattern);
        return NULL;
    }
    p++;

    const char *value_start = NULL;
    while (*p) {
        while (*p && is_json_ws(*p)) p++;
        if (*p == '}' || *p == 0) break;
        if (*p == ',') { p++; continue; }
        if (*p != '"') { p++; continue; }

        if (strncmp(p, pattern, pat_len) == 0) {
            const char *after = p + pat_len;
            while (*after && is_json_ws(*after)) after++;
            if (*after == ':') {
                after++;
                while (*after && is_json_ws(*after)) after++;
                value_start = after;
                break;
            }
        }

        p = skip_string(p);

        while (*p && is_json_ws(*p)) p++;
        if (*p == ':') p++;
        while (*p && is_json_ws(*p)) p++;

        if (*p == '"') {
            p = skip_string(p);
        } else if (*p == '{' || *p == '[') {
            p = skip_container(p);
        } else if (*p) {
            p = skip_scalar(p);
        }
    }

    if (pattern != stack_buf) free(pattern);
    return value_start;
}

char *json_get_string(const char *json, const char *key)
{
    const char *p = find_value_start(json, key);
    if (!p) return NULL;

    if (*p == '"') {
        p++;
        const char *end = p;
        while (*end && *end != '"') {
            if (*end == '\\' && *(end + 1)) end++;
            end++;
        }
        size_t len = (size_t)(end - p);
        char *val = (char *)malloc(len + 1);
        if (!val) return NULL;
        memcpy(val, p, len);
        val[len] = 0;
        return val;
    }
    const char *end = p;
    while (*end && *end != ',' && *end != '}' && *end != ']' && *end != '\n') end++;
    size_t len = (size_t)(end - p);
    while (len > 0 && (p[len - 1] == ' ' || p[len - 1] == '\r')) len--;
    char *val = (char *)malloc(len + 1);
    if (!val) return NULL;
    memcpy(val, p, len);
    val[len] = 0;
    return val;
}

static const char *locate_value(const char *json, const char *key, size_t *out_len)
{
    const char *p = find_value_start(json, key);
    if (!p) return NULL;

    if (*p == '"') {
        p++;
        const char *end = p;
        while (*end && *end != '"') {
            if (*end == '\\' && *(end + 1)) end++;
            end++;
        }
        *out_len = (size_t)(end - p);
        return p;
    }
    const char *end = p;
    while (*end && *end != ',' && *end != '}' && *end != ']' && *end != '\n') end++;
    size_t len = (size_t)(end - p);
    while (len > 0 && (p[len - 1] == ' ' || p[len - 1] == '\r')) len--;
    *out_len = len;
    return p;
}

double json_get_double(const char *json, const char *key, double dflt)
{
    size_t len;
    const char *span = locate_value(json, key, &len);
    if (!span || len == 0) return dflt;

    char buf[64];
    if (len >= sizeof(buf)) return dflt;
    memcpy(buf, span, len);
    buf[len] = '\0';

    char *endp;
    errno = 0;
    double r = strtod(buf, &endp);
    if (endp == buf || errno == ERANGE) return dflt;
    return r;
}

int json_get_int(const char *json, const char *key, int dflt)
{
    size_t len;
    const char *span = locate_value(json, key, &len);
    if (!span || len == 0) return dflt;

    char buf[32];
    if (len >= sizeof(buf)) return dflt;
    memcpy(buf, span, len);
    buf[len] = '\0';

    char *endp;
    errno = 0;
    long r = strtol(buf, &endp, 10);
    if (endp == buf || errno == ERANGE || r > INT_MAX || r < INT_MIN) return dflt;
    return (int)r;
}

const char *json_array_nth(const char *json, int n)
{
    if (!json) return NULL;
    const char *p = strchr(json, '[');
    if (!p) return NULL;
    p++;
    int depth = 0, idx = 0, in_str = 0;
    for (; *p; p++) {
        if (in_str) {
            if (in_str == 2) { in_str = 1; continue; }
            if (*p == '\\') { in_str = 2; continue; }
            if (*p == '"') { in_str = 0; }
            continue;
        }
        if (*p == '"') { in_str = 1; continue; }
        if (*p == '{') {
            if (depth == 0 && idx == n) return p;
            depth++;
        } else if (*p == '}') {
            depth--;
        } else if (*p == ',' && depth == 0) {
            idx++;
        } else if (*p == ']' && depth == 0) {
            break;
        }
    }
    return NULL;
}

char *json_extract_object(const char *start)
{
    if (!start || *start != '{') return NULL;
    int depth = 0, in_str = 0;
    const char *p = start;
    for (; *p; p++) {
        if (in_str) {
            if (in_str == 2) { in_str = 1; continue; }
            if (*p == '\\') { in_str = 2; continue; }
            if (*p == '"') { in_str = 0; }
            continue;
        }
        if (*p == '"') { in_str = 1; continue; }
        if (*p == '{') depth++;
        else if (*p == '}') { depth--; if (depth == 0) break; }
    }
    if (depth != 0) return NULL;
    size_t len = (size_t)(p - start + 1);
    char *obj = (char *)malloc(len + 1);
    if (!obj) return NULL;
    memcpy(obj, start, len);
    obj[len] = 0;
    return obj;
}
