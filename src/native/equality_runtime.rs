//! Structural equality for descriptor-backed values in the linked native runtime.
//!
//! Compare initialized fields, never padding, reference counts, or spare buffer capacity.
pub(super) const SOURCE: &str = r#"
unsafe fn equal_slot(left: usize, right: usize, semantic: u8) -> bool {
    unsafe {
        match semantic {
            0 => true,
            1 | 5 => *(left as *const u8) == *(right as *const u8),
            2 => (left as *const i64).read_unaligned() == (right as *const i64).read_unaligned(),
            3 => (left as *const f64).read_unaligned() == (right as *const f64).read_unaligned(),
            4 => (left as *const u32).read_unaligned() == (right as *const u32).read_unaligned(),
            6 | 7 => string_value(runtime_word(left)) == string_value(runtime_word(right)),
            8 | 10 => equal_object(runtime_word(left), runtime_word(right)),
            9 => runtime_word(left) == runtime_word(right),
            _ => unreachable!("invalid native value semantic"),
        }
    }
}

unsafe fn equal_fields(left: usize, right: usize, fields: &[RuntimeField]) -> bool {
    fields.iter().all(|field| unsafe {
        equal_slot(left + field.offset, right + field.offset, field.value.semantic)
    })
}

unsafe fn equal_object(left: usize, right: usize) -> bool {
    unsafe {
        if left == 0 || right == 0 { return left == right; }
        let descriptor = runtime_word(left);
        if descriptor != runtime_word(right) { return false; }
        let mut offset = 6;
        let kind = descriptor_u16(descriptor, &mut offset);
        offset = 36;
        match kind {
            0 => {
                let _name = descriptor_text(descriptor, &mut offset);
                equal_fields(left, right, &descriptor_fields(descriptor, &mut offset))
            }
            1 => {
                let _name = descriptor_text(descriptor, &mut offset);
                let tag_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let tag = ((left + tag_offset) as *const u32).read_unaligned();
                if tag != ((right + tag_offset) as *const u32).read_unaligned() { return false; }
                offset += 12; // payload offset, size, alignment, and reserved bits
                let alternatives = descriptor_u32(descriptor, &mut offset);
                for _ in 0..alternatives {
                    let _name = descriptor_text(descriptor, &mut offset);
                    let candidate = descriptor_u32(descriptor, &mut offset);
                    offset += 8; // payload size, alignment, and reserved bits
                    let fields = descriptor_fields(descriptor, &mut offset);
                    if candidate == tag { return equal_fields(left, right, &fields); }
                }
                unreachable!("invalid native variant tag")
            }
            2 => {
                let code_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let _signature_offset = descriptor_u32(descriptor, &mut offset);
                runtime_word(left + code_offset) == runtime_word(right + code_offset)
                    && equal_fields(left, right, &descriptor_fields(descriptor, &mut offset))
            }
            4 | 5 => {
                let data_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let length_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let element = if kind == 5 {
                    let _capacity_offset = descriptor_u32(descriptor, &mut offset);
                    descriptor_value(descriptor, &mut offset)
                } else { RuntimeValueLayout { size: 1, semantic: 5 } };
                let length = runtime_word(left + length_offset);
                if length != runtime_word(right + length_offset) { return false; }
                let left_data = runtime_word(left + data_offset);
                let right_data = runtime_word(right + data_offset);
                (0..length).all(|index| equal_slot(
                    left_data + index * element.size,
                    right_data + index * element.size,
                    element.semantic,
                ))
            }
            6 => {
                let handle_offset = descriptor_u32(descriptor, &mut offset) as usize;
                runtime_word(left + handle_offset) == runtime_word(right + handle_offset)
            }
            7 => {
                let code_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let environment_offset = descriptor_u32(descriptor, &mut offset) as usize;
                runtime_word(left + code_offset) == runtime_word(right + code_offset)
                    && equal_object(runtime_word(left + environment_offset), runtime_word(right + environment_offset))
            }
            8 => {
                let value_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let _release_offset = descriptor_u32(descriptor, &mut offset);
                let semantic_offset = descriptor_u32(descriptor, &mut offset) as usize;
                let semantic = *((left + semantic_offset) as *const u8);
                semantic == *((right + semantic_offset) as *const u8)
                    && equal_slot(left + value_offset, right + value_offset, semantic)
            }
            3 => left == right,
            _ => unreachable!("invalid native layout kind"),
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn foster_rt_v2_object_equal(left: usize, right: usize) -> u8 {
    u8::from(unsafe { equal_object(left, right) })
}
"#;
