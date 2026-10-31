"""Print, per method of a .class file, the field and method references its bytecode uses, in order.

Stdlib only; enough of the JVM class-file format to read the constant pool and walk the Code attribute.
Usage: python -I classcalls.py <file.class> [method-name-filter]
"""
import struct
import sys

# Instruction lengths for the opcodes we must skip correctly (others are 1 byte).
OPERAND_BYTES = {
    0x10: 1, 0x11: 2, 0x12: 1, 0x13: 2, 0x14: 2, 0x15: 1, 0x16: 1, 0x17: 1, 0x18: 1, 0x19: 1,
    0x36: 1, 0x37: 1, 0x38: 1, 0x39: 1, 0x3A: 1, 0x84: 2, 0xA9: 1, 0xB2: 2, 0xB3: 2, 0xB4: 2,
    0xB5: 2, 0xB6: 2, 0xB7: 2, 0xB8: 2, 0xB9: 4, 0xBA: 4, 0xBB: 2, 0xBC: 1, 0xBD: 2, 0xC0: 2,
    0xC1: 2, 0xC5: 3, 0xC6: 2, 0xC7: 2, 0xC8: 4, 0xC9: 4,
}
OPERAND_BYTES.update({op: 2 for op in range(0x99, 0xA9)})
REF_OPS = {0xB2: "getstatic", 0xB3: "putstatic", 0xB4: "getfield", 0xB5: "putfield",
           0xB6: "invokevirtual", 0xB7: "invokespecial", 0xB8: "invokestatic",
           0xB9: "invokeinterface", 0xBA: "invokedynamic"}


def main() -> None:
    data = open(sys.argv[1], "rb").read()
    flt = sys.argv[2] if len(sys.argv) > 2 else None
    pos = 8
    (count,) = struct.unpack_from(">H", data, pos)
    pos += 2
    cp: list = [None] * count
    i = 1
    while i < count:
        tag = data[pos]
        pos += 1
        if tag == 1:
            (n,) = struct.unpack_from(">H", data, pos)
            cp[i] = data[pos + 2:pos + 2 + n].decode("utf-8", "replace")
            pos += 2 + n
        elif tag in (3, 4):
            pos += 4
        elif tag in (5, 6):
            pos += 8
            i += 1
        elif tag in (7, 8, 16, 19, 20):
            cp[i] = (tag, struct.unpack_from(">H", data, pos)[0])
            pos += 2
        elif tag in (9, 10, 11, 12, 17, 18):
            cp[i] = (tag,) + struct.unpack_from(">HH", data, pos)
            pos += 4
        elif tag == 15:
            cp[i] = (tag, data[pos], struct.unpack_from(">H", data, pos + 1)[0])
            pos += 3
        else:
            raise SystemExit(f"unknown constant tag {tag}")
        i += 1

    def name_type(idx):
        _, n, t = cp[idx]
        return f"{cp[n]}{cp[t]}"

    def ref(idx):
        entry = cp[idx]
        if entry[0] in (9, 10, 11):
            cls = cp[cp[entry[1]][1]]
            return f"{cls}.{name_type(entry[2])}"
        if entry[0] == 18:  # invokedynamic: bootstrap index, name-and-type
            return f"indy:{name_type(entry[2])}"
        return str(entry)

    pos += 6
    (n_if,) = struct.unpack_from(">H", data, pos)
    pos += 2 + 2 * n_if

    def skip_attrs(p):
        (n,) = struct.unpack_from(">H", data, p)
        p += 2
        for _ in range(n):
            (ln,) = struct.unpack_from(">I", data, p + 2)
            p += 6 + ln
        return p

    (n_fields,) = struct.unpack_from(">H", data, pos)
    pos += 2
    for _ in range(n_fields):
        pos = skip_attrs(pos + 6)
    (n_methods,) = struct.unpack_from(">H", data, pos)
    pos += 2
    for _ in range(n_methods):
        _, name_i, desc_i, n_attr = struct.unpack_from(">HHHH", data, pos)
        pos += 8
        name = cp[name_i] + cp[desc_i]
        for _ in range(n_attr):
            a_name, a_len = struct.unpack_from(">HI", data, pos)
            body = data[pos + 6:pos + 6 + a_len]
            pos += 6 + a_len
            if cp[a_name] != "Code" or (flt and flt not in name):
                continue
            (code_len,) = struct.unpack_from(">I", body, 4)
            code = body[8:8 + code_len]
            print(f"== {name}")
            pc = 0
            while pc < len(code):
                op = code[pc]
                if op in REF_OPS:
                    (idx,) = struct.unpack_from(">H", code, pc + 1)
                    print(f"  {REF_OPS[op]:15} {ref(idx)}")
                if op == 0xAA:  # tableswitch
                    p = (pc + 4) & ~3
                    low, high = struct.unpack_from(">ii", code, p + 4)
                    pc = p + 12 + 4 * (high - low + 1)
                    continue
                if op == 0xAB:  # lookupswitch
                    p = (pc + 4) & ~3
                    (npairs,) = struct.unpack_from(">i", code, p + 4)
                    pc = p + 8 + 8 * npairs
                    continue
                if op == 0xC4:  # wide
                    pc += 6 if code[pc + 1] == 0x84 else 4
                    continue
                pc += 1 + OPERAND_BYTES.get(op, 0)


if __name__ == "__main__":
    main()
