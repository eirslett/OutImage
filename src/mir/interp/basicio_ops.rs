//! BASICIO and environment ops for the MIR interpreter (Phase 5).

use crate::basicio::{self, SYSIN_IDENTITY, SYSOUT_IDENTITY};
use crate::error::CompileError;
use crate::mir::{Function, LocalId, MirType, Op};
use crate::runtime::environment;
use crate::runtime::fs;
use crate::runtime::text::TextFrame;

use super::{
    ArrayStorage, ExecResult, SlotTag, Value, Vm, expect_array, expect_f64, expect_i64,
    expect_text, i64_to_char,
};

impl<'a> Vm<'a> {
    pub(super) fn init_basicio(&mut self) {
        self.basicio.ensure_terminals();
        // Keep `Vm::sysin` aligned with §10 `SYSIN.open(blanks(80))`. Without
        // this, `sysin.image.sub(1,5)` aborts with "sub out of frame" on an
        // empty ImageBuffer (simtst88). SYSOUT stays sparse: OutText grows the
        // image, and collected output must not include a pre-filled blank line.
        if let Ok(image) = TextFrame::blanks(basicio::DEFAULT_INPUT_LINELENGTH) {
            self.sysin.image_mut().load_line(&image.content());
            self.sysin
                .image_mut()
                .setpos((image.length as usize).saturating_add(1));
        }
    }

    pub(super) fn execute_basicio_or_env(
        &mut self,
        frame_index: usize,
        function: &Function,
        op: &Op,
    ) -> Result<ExecResult, CompileError> {
        match op {
            Op::CallSysIn { dest } => {
                let index = self.ensure_sysin_object()?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(index));
                Ok(ExecResult::Continue)
            }
            Op::CallSysOut { dest } => {
                let index = self.ensure_sysout_object()?;
                self.frames[frame_index].set_local(*dest, Value::ObjectRef(index));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioRegisterFile { object, path, mode } => {
                self.register_basicio_file(frame_index, *object, *path, *mode)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOpen {
                dest,
                object,
                fileimage,
            } => {
                let ok = self.basicio_open(frame_index, *object, *fileimage)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(ok));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOpenByte { dest, object } => {
                let ok = self.basicio_open_byte(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(ok));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioClose { dest, object } => {
                let ok = self.basicio_close(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(ok));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioIsOpen { dest, object } => {
                let open = self.basicio_is_open(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(open));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutText { object, text } => {
                let text = expect_text(
                    self.frames[frame_index].get_local(*text)?,
                    "CallBasicioOutText text",
                )?;
                self.basicio_out_text(frame_index, *object, &text.content())?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutChar { object, ch } => {
                let ch = i64_to_char(expect_i64(
                    self.frames[frame_index].get_local(*ch)?,
                    "CallBasicioOutChar ch",
                )?)?;
                self.basicio_out_char(frame_index, *object, ch)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutImage { object } => {
                self.basicio_out_image(frame_index, *object)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioBreakOutImage { object } => {
                self.basicio_break_out_image(frame_index, *object)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInImage { object } => {
                return self.basicio_in_image(frame_index, *object);
            }
            Op::CallBasicioInChar { dest, object } => {
                match self.basicio_in_char(frame_index, *object)? {
                    Some(ch) => {
                        self.frames[frame_index].set_local(*dest, Value::I64(ch as i64));
                        Ok(ExecResult::Continue)
                    }
                    None => Ok(ExecResult::NeedStdin),
                }
            }
            Op::CallBasicioLastItem { dest, object } => {
                let at_end = self.basicio_lastitem(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(at_end));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInInt { dest, object } => {
                let value = self.basicio_in_int(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(value));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInReal { dest, object } => {
                let value = self.basicio_in_real(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::F64(value));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInFrac { dest, object } => {
                let value = self.basicio_in_frac(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(value));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInText {
                dest,
                object,
                width,
            } => {
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallBasicioInText width",
                )?;
                let text = self.basicio_in_text(frame_index, *object, width)?;
                self.frames[frame_index].set_local(*dest, Value::Text(text));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioEndfile { dest, object } => {
                let endfile = self.basicio_endfile(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(endfile));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInByte { dest, object } => {
                let value = self.basicio_in_byte(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(value));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutByte { object, value } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallBasicioOutByte value",
                )?;
                self.basicio_out_byte(frame_index, *object, value)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLocate { object, loc } => {
                let loc = expect_i64(
                    self.frames[frame_index].get_local(*loc)?,
                    "CallBasicioLocate loc",
                )?;
                self.basicio_locate(frame_index, *object, loc)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLocation { dest, object } => {
                let loc = self.basicio_location(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(loc));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLastloc { dest, object } => {
                let loc = self.basicio_lastloc(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(loc));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutReal {
                object,
                value,
                digits,
                width,
                exp_digits,
            } => {
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallBasicioOutReal value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallBasicioOutReal digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallBasicioOutReal width",
                )?;
                let text = format_basicio_outreal(value, digits, width, *exp_digits)?;
                self.basicio_out_text(frame_index, *object, &text)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutFix {
                object,
                value,
                digits,
                width,
            } => {
                let value = expect_f64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallBasicioOutFix value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallBasicioOutFix digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallBasicioOutFix width",
                )?;
                let text = basicio::format_outfix_field(value, digits, width)
                    .map_err(CompileError::codegen)?;
                self.basicio_out_text(frame_index, *object, &text)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutFrac {
                object,
                value,
                digits,
                width,
            } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallBasicioOutFrac value",
                )?;
                let digits = expect_i64(
                    self.frames[frame_index].get_local(*digits)?,
                    "CallBasicioOutFrac digits",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallBasicioOutFrac width",
                )?;
                let text = super::format_out_frac(value, digits, width)?;
                self.basicio_out_text(frame_index, *object, &text)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioOutInt {
                object,
                value,
                width,
            } => {
                let value = expect_i64(
                    self.frames[frame_index].get_local(*value)?,
                    "CallBasicioOutInt value",
                )?;
                let width = expect_i64(
                    self.frames[frame_index].get_local(*width)?,
                    "CallBasicioOutInt width",
                )?;
                let text =
                    basicio::format_outint_field(value, width).map_err(CompileError::codegen)?;
                self.basicio_out_text(frame_index, *object, &text)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLine { dest, object } => {
                let line = self.basicio_line(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(line));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioImage { dest, object } => {
                let image = self.basicio_image(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Text(image));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioPos { dest, object } => {
                let pos = self.basicio_pos(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(pos));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLength { dest, object } => {
                let len = self.basicio_length(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::I64(len));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioSetImage { object, text } => {
                let text = expect_text(
                    self.frames[frame_index].get_local(*text)?,
                    "CallBasicioSetImage text",
                )?
                .clone();
                self.basicio_set_image(frame_index, *object, &text)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioSetpos { object, index } => {
                let index = expect_i64(
                    self.frames[frame_index].get_local(*index)?,
                    "CallBasicioSetpos index",
                )?;
                self.basicio_setpos(frame_index, *object, index)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioFilename { dest, object } => {
                let name = self.basicio_filename(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Text(name));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioSetAccess { dest, object, mode } => {
                let mode = expect_text(
                    self.frames[frame_index].get_local(*mode)?,
                    "CallBasicioSetAccess mode",
                )?;
                let ok = self.basicio_set_access(frame_index, *object, &mode.content())?;
                self.frames[frame_index].set_local(*dest, Value::Bool(ok));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioEject { object, line } => {
                let line = expect_i64(
                    self.frames[frame_index].get_local(*line)?,
                    "CallBasicioEject line",
                )?;
                self.basicio_eject(frame_index, *object, line)?;
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioLinesPerPage { dest, object, n } => {
                let n = expect_i64(
                    self.frames[frame_index].get_local(*n)?,
                    "CallBasicioLinesPerPage n",
                )?;
                let prev = self.basicio_linesperpage(frame_index, *object, n)?;
                self.frames[frame_index].set_local(*dest, Value::I64(prev));
                Ok(ExecResult::Continue)
            }
            Op::CallBasicioInRecord { dest, object } => {
                let truncated = self.basicio_in_record(frame_index, *object)?;
                self.frames[frame_index].set_local(*dest, Value::Bool(truncated));
                Ok(ExecResult::Continue)
            }
            Op::CallEnv { dest, name, args } => {
                let dest_ty = function.local(*dest).ty;
                let value = self.call_env(frame_index, name, args, dest_ty)?;
                self.frames[frame_index].set_local(*dest, value);
                Ok(ExecResult::Continue)
            }
            Op::CallFileExists { dest, path } => {
                let path = expect_text(
                    self.frames[frame_index].get_local(*path)?,
                    "CallFileExists path",
                )?;
                let exists = fs::exists(&path.content());
                self.frames[frame_index].set_local(*dest, Value::Bool(exists));
                Ok(ExecResult::Continue)
            }
            Op::CallFileRead { dest, path } => {
                let path = expect_text(
                    self.frames[frame_index].get_local(*path)?,
                    "CallFileRead path",
                )?;
                let contents = fs::read_file(&path.content())
                    .map_err(|error| CompileError::codegen(error.to_string()))?;
                self.frames[frame_index]
                    .set_local(*dest, Value::Text(TextFrame::from_literal(&contents, true)));
                Ok(ExecResult::Continue)
            }
            Op::CallFileWrite { path, contents } => {
                let path = expect_text(
                    self.frames[frame_index].get_local(*path)?,
                    "CallFileWrite path",
                )?;
                let contents = expect_text(
                    self.frames[frame_index].get_local(*contents)?,
                    "CallFileWrite contents",
                )?;
                fs::write_file(&path.content(), &contents.content())
                    .map_err(|error| CompileError::codegen(error.to_string()))?;
                Ok(ExecResult::Continue)
            }
            _ => unreachable!("execute_basicio_or_env called with non-basicio op"),
        }
    }

    fn ensure_sysin_object(&mut self) -> Result<usize, CompileError> {
        if let Some(index) = self.sysin_object {
            return Ok(index);
        }
        self.basicio.ensure_terminals();
        let index = self.alloc_object(8, 0)?;
        self.object_identities.insert(index, SYSIN_IDENTITY);
        self.sysin_object = Some(index);
        Ok(index)
    }

    fn ensure_sysout_object(&mut self) -> Result<usize, CompileError> {
        if let Some(index) = self.sysout_object {
            return Ok(index);
        }
        self.basicio.ensure_terminals();
        let index = self.alloc_object(8, 0)?;
        self.object_identities.insert(index, SYSOUT_IDENTITY);
        self.sysout_object = Some(index);
        Ok(index)
    }

    fn object_identity(&self, object_index: usize) -> Result<u64, CompileError> {
        self.object_identities
            .get(&object_index)
            .copied()
            .ok_or_else(|| {
                CompileError::codegen(format!(
                    "MIR interp: object {object_index} is not registered as a BASICIO file"
                ))
            })
    }

    fn identity_for_object_local(
        &self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<u64, CompileError> {
        let index =
            self.object_index(frame_index, object, "remote access through none reference")?;
        self.object_identity(index)
    }

    fn register_basicio_file(
        &mut self,
        frame_index: usize,
        object: LocalId,
        path: LocalId,
        mode: i64,
    ) -> Result<(), CompileError> {
        let object_index =
            self.object_index(frame_index, object, "BASICIO register: none object")?;
        let path_text = expect_text(
            self.frames[frame_index].get_local(path)?,
            "CallBasicioRegisterFile path",
        )?;
        let filename = path_text.content().to_string();
        if filename.is_empty() {
            return Err(CompileError::runtime("file: FILENAME is notext"));
        }
        let class_name = class_name_for_mode(mode);
        let identity = self
            .object_identities
            .entry(object_index)
            .or_insert_with(|| {
                let id = self.next_file_identity;
                self.next_file_identity += 1;
                id
            });
        basicio::register_file_object(&mut self.basicio, *identity, class_name, filename)
            .map_err(CompileError::codegen)?;
        Ok(())
    }

    fn basicio_open(
        &mut self,
        frame_index: usize,
        object: LocalId,
        fileimage: LocalId,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSIN_IDENTITY || identity == SYSOUT_IDENTITY {
            return Ok(true);
        }
        let image = expect_text(
            self.frames[frame_index].get_local(fileimage)?,
            "CallBasicioOpen fileimage",
        )?
        .clone();
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: open: unknown BASICIO file object")
        })?;
        basicio::open_file(handle, image).map_err(CompileError::codegen)
    }

    fn basicio_open_byte(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: open_byte: unknown BASICIO file object")
        })?;
        basicio::open_bytefile(handle).map_err(CompileError::codegen)
    }

    fn basicio_close(&mut self, frame_index: usize, object: LocalId) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSIN_IDENTITY || identity == SYSOUT_IDENTITY {
            return Ok(true);
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: close: unknown BASICIO file object")
        })?;
        basicio::close_file(handle).map_err(CompileError::codegen)
    }

    fn basicio_is_open(&self, frame_index: usize, object: LocalId) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.open)
            .unwrap_or(false))
    }

    fn basicio_out_text(
        &mut self,
        frame_index: usize,
        object: LocalId,
        text: &str,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.sysout.out_text(text);
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: outtext: unknown BASICIO file object")
        })?;
        basicio::file_out_text(handle, text).map_err(CompileError::codegen)
    }

    fn basicio_out_char(
        &mut self,
        frame_index: usize,
        object: LocalId,
        ch: char,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.sysout.out_char(ch);
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: outchar: unknown BASICIO file object")
        })?;
        basicio::file_out_char(handle, ch).map_err(CompileError::codegen)
    }

    fn basicio_out_image(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.emit_out_image();
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: outimage: unknown BASICIO file object")
        })?;
        basicio::file_out_image(handle).map_err(CompileError::codegen)
    }

    fn basicio_break_out_image(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.emit_break_out_image();
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: breakoutimage: unknown BASICIO file object")
        })?;
        basicio::file_break_out_image(handle).map_err(CompileError::codegen)
    }

    fn basicio_in_image(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<ExecResult, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSIN_IDENTITY {
            return self.apply_stdin();
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inimage: unknown BASICIO file object")
        })?;
        basicio::file_in_image(handle).map_err(CompileError::codegen)?;
        Ok(ExecResult::Continue)
    }

    fn basicio_in_char(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<Option<char>, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSIN_IDENTITY {
            // Match `file_in_char`: refill from stdin when the image is exhausted.
            if self.sysin.image().endfile() {
                return Err(CompileError::runtime("InChar: end of file"));
            }
            let at_end = self.sysin.image().pos() > self.sysin.image().length();
            if at_end {
                match self.apply_stdin()? {
                    ExecResult::NeedStdin => return Ok(None),
                    ExecResult::Continue => {}
                    other => {
                        return Err(CompileError::codegen(format!(
                            "MIR interp: unexpected control flow during InChar ({other:?})"
                        )));
                    }
                }
            }
            return self
                .sysin
                .in_char()
                .map(Some)
                .map_err(CompileError::codegen);
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inchar: unknown BASICIO file object")
        })?;
        basicio::file_in_char(handle)
            .map(Some)
            .map_err(CompileError::codegen)
    }

    fn basicio_lastitem(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: lastitem: unknown BASICIO file object")
        })?;
        basicio::file_lastitem(handle).map_err(CompileError::codegen)
    }

    fn basicio_in_int(&mut self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inint: unknown BASICIO file object")
        })?;
        basicio::file_in_int(handle).map_err(CompileError::codegen)
    }

    fn basicio_in_real(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<f64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inreal: unknown BASICIO file object")
        })?;
        basicio::file_in_real(handle).map_err(CompileError::codegen)
    }

    fn basicio_in_frac(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: infrac: unknown BASICIO file object")
        })?;
        basicio::file_in_frac(handle).map_err(CompileError::codegen)
    }

    fn basicio_in_text(
        &mut self,
        frame_index: usize,
        object: LocalId,
        width: i64,
    ) -> Result<TextFrame, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: intext: unknown BASICIO file object")
        })?;
        if handle.kind.is_byte() {
            let frame = TextFrame::blanks(width.max(0)).map_err(CompileError::codegen)?;
            return basicio::file_byte_in_text(handle, frame).map_err(CompileError::codegen);
        }
        basicio::file_in_text(handle, width).map_err(CompileError::codegen)
    }

    fn basicio_endfile(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSIN_IDENTITY {
            return Ok(self.sysin.endfile());
        }
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.endfile)
            .unwrap_or(true))
    }

    fn basicio_in_byte(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inbyte: unknown BASICIO file object")
        })?;
        basicio::file_in_byte(handle).map_err(CompileError::codegen)
    }

    fn basicio_out_byte(
        &mut self,
        frame_index: usize,
        object: LocalId,
        value: i64,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: outbyte: unknown BASICIO file object")
        })?;
        basicio::file_out_byte(handle, value).map_err(CompileError::codegen)
    }

    fn basicio_locate(
        &mut self,
        frame_index: usize,
        object: LocalId,
        loc: i64,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: locate: unknown BASICIO file object")
        })?;
        basicio::file_locate(handle, loc).map_err(CompileError::codegen)
    }

    fn basicio_location(&self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.loc)
            .unwrap_or(0))
    }

    fn basicio_lastloc(&self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: lastloc: unknown BASICIO file object")
        })?;
        basicio::file_lastloc(handle).map_err(CompileError::codegen)
    }

    fn basicio_line(&self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.line)
            .unwrap_or(1))
    }

    fn basicio_image(
        &self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<TextFrame, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            let content = self.sysout.image().content().to_string();
            return Ok(TextFrame::from_literal(&content, true));
        }
        if identity == SYSIN_IDENTITY {
            let content = self.sysin.image().content().to_string();
            return Ok(TextFrame::from_literal(&content, true));
        }
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.image.clone())
            .unwrap_or_else(TextFrame::notext))
    }

    fn basicio_pos(&self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            return Ok(self.sysout.image().pos() as i64);
        }
        if identity == SYSIN_IDENTITY {
            return Ok(self.sysin.image().pos() as i64);
        }
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.image.pos)
            .unwrap_or(1))
    }

    fn basicio_length(&self, frame_index: usize, object: LocalId) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            return Ok(self.sysout.image().length() as i64);
        }
        if identity == SYSIN_IDENTITY {
            return Ok(self.sysin.image().content().chars().count() as i64);
        }
        Ok(self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.image.length)
            .unwrap_or(0))
    }

    fn basicio_set_image(
        &mut self,
        frame_index: usize,
        object: LocalId,
        text: &TextFrame,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.sysout.image_mut().load_line(&text.content());
            return Ok(());
        }
        if identity == SYSIN_IDENTITY {
            self.sysin.image_mut().load_line(&text.content());
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: setimage: unknown BASICIO file object")
        })?;
        handle
            .image
            .assign_value_from(text)
            .map_err(CompileError::codegen)?;
        handle.image.setpos(1);
        Ok(())
    }

    fn basicio_setpos(
        &mut self,
        frame_index: usize,
        object: LocalId,
        index: i64,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            self.sysout.image_mut().setpos(index.max(1) as usize);
            return Ok(());
        }
        if identity == SYSIN_IDENTITY {
            self.sysin.image_mut().setpos(index.max(1) as usize);
            return Ok(());
        }
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: setpos: unknown BASICIO file object")
        })?;
        handle.image.setpos(index);
        Ok(())
    }

    fn basicio_filename(
        &self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<TextFrame, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        if identity == SYSOUT_IDENTITY {
            return Ok(TextFrame::from_literal("<SYSOUT>", true));
        }
        if identity == SYSIN_IDENTITY {
            return Ok(TextFrame::from_literal("<SYSIN>", true));
        }
        let name = self
            .basicio
            .files
            .get(&identity)
            .map(|h| h.filename.clone())
            .unwrap_or_default();
        Ok(TextFrame::from_literal(&name, true))
    }

    fn basicio_set_access(
        &mut self,
        frame_index: usize,
        object: LocalId,
        mode: &str,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: setaccess: unknown BASICIO file object")
        })?;
        Ok(basicio::set_access(handle, mode))
    }

    fn basicio_eject(
        &mut self,
        frame_index: usize,
        object: LocalId,
        line: i64,
    ) -> Result<(), CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: eject: unknown BASICIO file object")
        })?;
        basicio::file_eject(handle, line).map_err(CompileError::codegen)
    }

    fn basicio_linesperpage(
        &mut self,
        frame_index: usize,
        object: LocalId,
        n: i64,
    ) -> Result<i64, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: linesperpage: unknown BASICIO file object")
        })?;
        Ok(basicio::file_linesperpage(handle, n))
    }

    fn basicio_in_record(
        &mut self,
        frame_index: usize,
        object: LocalId,
    ) -> Result<bool, CompileError> {
        let identity = self.identity_for_object_local(frame_index, object)?;
        let handle = self.basicio.files.get_mut(&identity).ok_or_else(|| {
            CompileError::codegen("MIR interp: inrecord: unknown BASICIO file object")
        })?;
        basicio::file_in_record(handle).map_err(CompileError::codegen)
    }

    fn call_env(
        &mut self,
        frame_index: usize,
        name: &str,
        args: &[LocalId],
        dest_ty: MirType,
    ) -> Result<Value, CompileError> {
        let frame = &self.frames[frame_index];
        match name {
            "mod" => {
                let i = expect_i64(frame.get_local(args[0])?, "CallEnv mod i")?;
                let j = expect_i64(frame.get_local(args[1])?, "CallEnv mod j")?;
                Ok(Value::I64(
                    environment::mod_(i, j).map_err(CompileError::codegen)?,
                ))
            }
            "rem" => {
                let i = expect_i64(frame.get_local(args[0])?, "CallEnv rem i")?;
                let j = expect_i64(frame.get_local(args[1])?, "CallEnv rem j")?;
                Ok(Value::I64(
                    environment::rem(i, j).map_err(CompileError::codegen)?,
                ))
            }
            "abs_int" | "abs" | "abs_real" => match dest_ty {
                MirType::F64 | MirType::LongF64 => {
                    let r = expect_f64(frame.get_local(args[0])?, "CallEnv abs")?;
                    Ok(Value::F64(environment::abs_real(r)))
                }
                _ => {
                    let i = expect_i64(frame.get_local(args[0])?, "CallEnv abs")?;
                    Ok(Value::I64(environment::abs_integer(i)))
                }
            },
            "sign" => {
                let value = frame.get_local(args[0])?;
                match value {
                    Value::F64(r) => Ok(Value::I64(environment::sign_real(*r))),
                    Value::I64(i) => Ok(Value::I64(environment::sign(*i))),
                    other => Err(CompileError::codegen(format!(
                        "CallEnv sign: expected numeric, got {other:?}"
                    ))),
                }
            }
            "sqrt" => {
                let r = expect_f64(frame.get_local(args[0])?, "CallEnv sqrt")?;
                Ok(Value::F64(
                    environment::sqrt_real(r).map_err(CompileError::codegen)?,
                ))
            }
            "sin" => Ok(Value::F64(environment::sin_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv sin",
            )?))),
            "cos" => Ok(Value::F64(environment::cos_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv cos",
            )?))),
            "tan" => Ok(Value::F64(environment::tan_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv tan",
            )?))),
            "cotan" => Ok(Value::F64(environment::cotan_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv cotan",
            )?))),
            "arcsin" => Ok(Value::F64(
                environment::arcsin_real(expect_f64(frame.get_local(args[0])?, "CallEnv arcsin")?)
                    .map_err(CompileError::codegen)?,
            )),
            "arccos" => Ok(Value::F64(
                environment::arccos_real(expect_f64(frame.get_local(args[0])?, "CallEnv arccos")?)
                    .map_err(CompileError::codegen)?,
            )),
            "arctan" => Ok(Value::F64(environment::arctan_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv arctan",
            )?))),
            "arctan2" => {
                let y = expect_f64(frame.get_local(args[0])?, "CallEnv arctan2 y")?;
                let x = expect_f64(frame.get_local(args[1])?, "CallEnv arctan2 x")?;
                Ok(Value::F64(
                    environment::arctan2_real(y, x).map_err(CompileError::codegen)?,
                ))
            }
            "sinh" => Ok(Value::F64(environment::sinh_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv sinh",
            )?))),
            "cosh" => Ok(Value::F64(environment::cosh_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv cosh",
            )?))),
            "tanh" => Ok(Value::F64(environment::tanh_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv tanh",
            )?))),
            "ln" => Ok(Value::F64(
                environment::ln_real(expect_f64(frame.get_local(args[0])?, "CallEnv ln")?)
                    .map_err(CompileError::codegen)?,
            )),
            "log10" => Ok(Value::F64(
                environment::log10_real(expect_f64(frame.get_local(args[0])?, "CallEnv log10")?)
                    .map_err(CompileError::codegen)?,
            )),
            "exp" => Ok(Value::F64(environment::exp_real(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv exp",
            )?))),
            "lowten" => {
                let ch = i64_to_char(expect_i64(frame.get_local(args[0])?, "CallEnv lowten")?)?;
                Ok(Value::I64(
                    environment::lowten(&mut self.env_state, ch).map_err(CompileError::codegen)?
                        as i64,
                ))
            }
            "decimalmark" => {
                let ch = i64_to_char(expect_i64(
                    frame.get_local(args[0])?,
                    "CallEnv decimalmark",
                )?)?;
                Ok(Value::I64(
                    environment::decimalmark(&mut self.env_state, ch)
                        .map_err(CompileError::codegen)? as i64,
                ))
            }
            "current_lowten" => Ok(Value::I64(self.env_state.current_lowten as i64)),
            "current_decimalmark" => Ok(Value::I64(self.env_state.current_decimal_mark as i64)),
            "draw" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv draw")?;
                let stream = self.load_stream_ref(frame_index, args[1])?;
                let mut stream = stream;
                let result = environment::draw(a, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[1], stream)?;
                Ok(Value::Bool(result))
            }
            "randint" => {
                let a = expect_i64(frame.get_local(args[0])?, "CallEnv randint a")?;
                let b = expect_i64(frame.get_local(args[1])?, "CallEnv randint b")?;
                let stream = self.load_stream_ref(frame_index, args[2])?;
                let mut stream = stream;
                let result =
                    environment::randint(a, b, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[2], stream)?;
                Ok(Value::I64(result))
            }
            "uniform" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv uniform a")?;
                let b = expect_f64(frame.get_local(args[1])?, "CallEnv uniform b")?;
                let stream = self.load_stream_ref(frame_index, args[2])?;
                let mut stream = stream;
                let result =
                    environment::uniform(a, b, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[2], stream)?;
                Ok(Value::F64(result))
            }
            "normal" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv normal a")?;
                let b = expect_f64(frame.get_local(args[1])?, "CallEnv normal b")?;
                let stream = self.load_stream_ref(frame_index, args[2])?;
                let mut stream = stream;
                let result =
                    environment::normal(a, b, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[2], stream)?;
                Ok(Value::F64(result))
            }
            "negexp" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv negexp a")?;
                let stream = self.load_stream_ref(frame_index, args[1])?;
                let mut stream = stream;
                let result = environment::negexp(a, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[1], stream)?;
                Ok(Value::F64(result))
            }
            "poisson" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv poisson a")?;
                let stream = self.load_stream_ref(frame_index, args[1])?;
                let mut stream = stream;
                let result = environment::poisson(a, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[1], stream)?;
                Ok(Value::I64(result))
            }
            "erlang" => {
                let a = expect_f64(frame.get_local(args[0])?, "CallEnv erlang a")?;
                let b = expect_f64(frame.get_local(args[1])?, "CallEnv erlang b")?;
                let stream = self.load_stream_ref(frame_index, args[2])?;
                let mut stream = stream;
                let result =
                    environment::erlang(a, b, &mut stream).map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[2], stream)?;
                Ok(Value::F64(result))
            }
            "discrete" | "histd" => {
                let array_index = expect_array(
                    self.frames[frame_index].get_local(args[0])?,
                    &format!("CallEnv {name}"),
                )?;
                let (lo, values) = self.dense_f64_1d(array_index)?;
                let stream = self.load_stream_ref(frame_index, args[1])?;
                let mut stream = stream;
                let one_based = if name == "discrete" {
                    environment::discrete(&values, &mut stream)
                } else {
                    environment::histd(&values, &mut stream)
                }
                .map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[1], stream)?;
                // Rust helpers assume a 1-based dense index; native returns lo+i.
                Ok(Value::I64(lo + one_based - 1))
            }
            "linear" => {
                let a_index = expect_array(
                    self.frames[frame_index].get_local(args[0])?,
                    "CallEnv linear a",
                )?;
                let b_index = expect_array(
                    self.frames[frame_index].get_local(args[1])?,
                    "CallEnv linear b",
                )?;
                let (_a_lo, a_vals) = self.dense_f64_1d(a_index)?;
                let (_b_lo, b_vals) = self.dense_f64_1d(b_index)?;
                let stream = self.load_stream_ref(frame_index, args[2])?;
                let mut stream = stream;
                let result = environment::linear(&a_vals, &b_vals, &mut stream)
                    .map_err(CompileError::codegen)?;
                self.store_stream_ref(frame_index, args[2], stream)?;
                Ok(Value::F64(result))
            }
            "histo" => {
                let a_index = expect_array(
                    self.frames[frame_index].get_local(args[0])?,
                    "CallEnv histo a",
                )?;
                let b_index = expect_array(
                    self.frames[frame_index].get_local(args[1])?,
                    "CallEnv histo b",
                )?;
                let c = expect_f64(
                    self.frames[frame_index].get_local(args[2])?,
                    "CallEnv histo c",
                )?;
                let d = expect_f64(
                    self.frames[frame_index].get_local(args[3])?,
                    "CallEnv histo d",
                )?;
                let (a_lo, mut a_vals) = self.dense_f64_1d(a_index)?;
                let (_b_lo, b_vals) = self.dense_f64_1d(b_index)?;
                environment::histo(&mut a_vals, &b_vals, c, d).map_err(CompileError::codegen)?;
                self.store_dense_f64_1d(a_index, a_lo, &a_vals)?;
                // Native `simrt_histo` returns 0; the MIR dest is unused for the statement.
                Ok(Value::I64(0))
            }
            "lowerbound" | "upperbound" => {
                let array_index = expect_array(
                    self.frames[frame_index].get_local(args[0])?,
                    &format!("CallEnv {name}"),
                )?;
                let dim = expect_i64(
                    self.frames[frame_index].get_local(args[1])?,
                    &format!("CallEnv {name} dim"),
                )?;
                Ok(Value::I64(self.array_bound(
                    array_index,
                    dim,
                    name == "upperbound",
                )?))
            }
            "error" => {
                let message = expect_text(
                    self.frames[frame_index].get_local(args[0])?,
                    "CallEnv error",
                )?
                .content();
                Err(CompileError::codegen(message))
            }
            "datetime" => Ok(Value::Text(environment::datetime_text())),
            "cputime" => Ok(Value::F64(environment::cputime(&self.env_state))),
            "clocktime" => Ok(Value::F64(environment::clocktime())),
            "max_int" => {
                if args.is_empty() {
                    Ok(Value::I64(environment::MAXINT))
                } else {
                    let a = expect_i64(frame.get_local(args[0])?, "CallEnv max_int a")?;
                    let b = expect_i64(frame.get_local(args[1])?, "CallEnv max_int b")?;
                    Ok(Value::I64(a.max(b)))
                }
            }
            "min_int" => {
                if args.is_empty() {
                    Ok(Value::I64(environment::MININT))
                } else {
                    let a = expect_i64(frame.get_local(args[0])?, "CallEnv min_int a")?;
                    let b = expect_i64(frame.get_local(args[1])?, "CallEnv min_int b")?;
                    Ok(Value::I64(a.min(b)))
                }
            }
            "max_real" => {
                if args.is_empty() {
                    Ok(Value::F64(environment::MAXREAL))
                } else {
                    let a = expect_f64(frame.get_local(args[0])?, "CallEnv max_real a")?;
                    let b = expect_f64(frame.get_local(args[1])?, "CallEnv max_real b")?;
                    Ok(Value::F64(environment::max_real(a, b)))
                }
            }
            "min_real" => {
                if args.is_empty() {
                    Ok(Value::F64(environment::MINREAL))
                } else {
                    let a = expect_f64(frame.get_local(args[0])?, "CallEnv min_real a")?;
                    let b = expect_f64(frame.get_local(args[1])?, "CallEnv min_real b")?;
                    Ok(Value::F64(environment::min_real(a, b)))
                }
            }
            "max_text" | "min_text" => {
                let a = expect_text(frame.get_local(args[0])?, "CallEnv max/min_text a")?.content();
                let b = expect_text(frame.get_local(args[1])?, "CallEnv max/min_text b")?.content();
                let result = if name == "max_text" {
                    environment::max_text(&a, &b)
                } else {
                    environment::min_text(&a, &b)
                };
                Ok(Value::Text(TextFrame::from_mutable(&result)))
            }
            "addepsilon" => Ok(Value::F64(environment::addepsilon(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv addepsilon",
            )?))),
            "subepsilon" => Ok(Value::F64(environment::subepsilon(expect_f64(
                frame.get_local(args[0])?,
                "CallEnv subepsilon",
            )?))),
            "digit" => {
                let ch = i64_to_char(expect_i64(frame.get_local(args[0])?, "CallEnv digit")?)?;
                Ok(Value::Bool(environment::digit_char(ch)))
            }
            "letter" => {
                let ch = i64_to_char(expect_i64(frame.get_local(args[0])?, "CallEnv letter")?)?;
                Ok(Value::Bool(environment::letter_char(ch)))
            }
            "char" => {
                let code = expect_i64(frame.get_local(args[0])?, "CallEnv char")?;
                Ok(Value::I64(
                    environment::char_code(code).map_err(CompileError::codegen)? as i64,
                ))
            }
            "isochar" => {
                let code = expect_i64(frame.get_local(args[0])?, "CallEnv isochar")?;
                Ok(Value::I64(
                    environment::isochar_code(code).map_err(CompileError::codegen)? as i64,
                ))
            }
            "rank" => {
                let ch = i64_to_char(expect_i64(frame.get_local(args[0])?, "CallEnv rank")?)?;
                Ok(Value::I64(environment::rank_char(ch)))
            }
            "isorank" => {
                let ch = i64_to_char(expect_i64(frame.get_local(args[0])?, "CallEnv isorank")?)?;
                Ok(Value::I64(environment::isorank_char(ch)))
            }
            other => Err(CompileError::codegen(format!(
                "MIR interp: CallEnv '{other}' not implemented"
            ))),
        }
    }

    /// 1-based dimension bound for `lowerbound` / `upperbound` (§9.8).
    fn array_bound(
        &self,
        array_index: usize,
        dim_1based: i64,
        upper: bool,
    ) -> Result<i64, CompileError> {
        let array = self.arrays.get(array_index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid array index {array_index}"))
        })?;
        let bounds = match array {
            ArrayStorage::I64 { bounds, .. }
            | ArrayStorage::F64 { bounds, .. }
            | ArrayStorage::Text { bounds, .. } => bounds,
            ArrayStorage::Free => {
                return Err(CompileError::codegen(
                    "MIR interp: array access through a collected array descriptor",
                ));
            }
        };
        if dim_1based < 1 || dim_1based as usize > bounds.len() {
            return Err(CompileError::runtime("array dimension out of range"));
        }
        let (lo, hi) = bounds[dim_1based as usize - 1];
        Ok(if upper { hi } else { lo })
    }

    /// Dense 1-D real slice in subscript order (missing sparse cells read as 0.0).
    fn dense_f64_1d(&self, array_index: usize) -> Result<(i64, Vec<f64>), CompileError> {
        let array = self.arrays.get(array_index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid array index {array_index}"))
        })?;
        match array {
            ArrayStorage::F64 { bounds, cells } => {
                if bounds.len() != 1 {
                    return Err(CompileError::codegen(
                        "real array argument must be one-dimensional",
                    ));
                }
                let (lo, hi) = bounds[0];
                let mut values = Vec::new();
                if hi >= lo {
                    let count = (hi - lo + 1) as usize;
                    values.reserve(count);
                    let mut i = lo;
                    while i <= hi {
                        values.push(cells.get(&vec![i]).copied().unwrap_or(0.0));
                        i += 1;
                    }
                }
                Ok((lo, values))
            }
            ArrayStorage::Free => Err(CompileError::codegen(
                "MIR interp: array access through a collected array descriptor",
            )),
            _ => Err(CompileError::codegen(
                "CallEnv: expected a real array argument",
            )),
        }
    }

    fn store_dense_f64_1d(
        &mut self,
        array_index: usize,
        lo: i64,
        values: &[f64],
    ) -> Result<(), CompileError> {
        let array = self.arrays.get_mut(array_index).ok_or_else(|| {
            CompileError::codegen(format!("MIR interp: invalid array index {array_index}"))
        })?;
        match array {
            ArrayStorage::F64 { cells, .. } => {
                for (offset, &value) in values.iter().enumerate() {
                    let key = vec![lo + offset as i64];
                    if value == 0.0 {
                        cells.remove(&key);
                    } else {
                        cells.insert(key, value);
                    }
                }
                Ok(())
            }
            ArrayStorage::Free => Err(CompileError::codegen(
                "MIR interp: array access through a collected array descriptor",
            )),
            _ => Err(CompileError::codegen(
                "CallEnv histo: expected a real array argument",
            )),
        }
    }

    fn load_stream_ref(&mut self, frame_index: usize, local: LocalId) -> Result<i64, CompileError> {
        let ptr = self.frames[frame_index].get_local(local)?.clone();
        self.load_ref_i64(&ptr, 0)
    }

    fn store_stream_ref(
        &mut self,
        frame_index: usize,
        local: LocalId,
        value: i64,
    ) -> Result<(), CompileError> {
        let ptr = self.frames[frame_index].get_local(local)?.clone();
        // Stream positions / flags are plain integers.
        self.store_ref_i64(&ptr, 0, value, SlotTag::Scalar)
    }
}

fn class_name_for_mode(mode: i64) -> &'static str {
    match mode {
        0 => "InFile",
        2 => "InByteFile",
        3 => "OutByteFile",
        4 => "DirectFile",
        5 => "DirectByteFile",
        6 => "PrintFile",
        _ => "OutFile",
    }
}

pub(super) fn format_basicio_outreal(
    value: f64,
    digits: i64,
    width: i64,
    exp_digits: i64,
) -> Result<String, CompileError> {
    let field_width = if width == 0 {
        let mut tmp = TextFrame::blanks(64).map_err(CompileError::codegen)?;
        if exp_digits == 3 {
            tmp.edit_putreal_long_with(value, digits, '.', '&')
        } else {
            tmp.edit_putreal(value, digits)
        }
        .map_err(CompileError::codegen)?;
        tmp.content().trim().chars().count().max(1) as i64
    } else {
        width.abs()
    };
    let mut field = TextFrame::blanks(field_width).map_err(CompileError::codegen)?;
    if exp_digits == 3 {
        field
            .edit_putreal_long_with(value, digits, '.', '&')
            .map_err(CompileError::codegen)?;
    } else {
        field
            .edit_putreal(value, digits)
            .map_err(CompileError::codegen)?;
    }
    Ok(field.content().to_string())
}
