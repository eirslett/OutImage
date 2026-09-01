export const HELLO = `begin
   OutText("hello world");
   OutImage;
end;
`;

export const STDIN_ECHO = `begin
   OutText("Type a line, then Enter:");
   OutImage;
   InImage;
   OutText("got it");
   OutImage;
end;
`;

export const TYPE_MISMATCH = `begin
   integer x;
   x := true;
end;
`;

export const EXAMPLES: { id: string; label: string; source: string }[] = [
  { id: "hello", label: "hello world", source: HELLO },
  { id: "stdin", label: "stdin prompt", source: STDIN_ECHO },
  { id: "mismatch", label: "type mismatch", source: TYPE_MISMATCH },
];
