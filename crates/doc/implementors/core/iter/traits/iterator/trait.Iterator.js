(function() {var implementors = {};
implementors["wasmer"] = [{"text":"impl&lt;'a, I&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/iter/traits/iterator/trait.Iterator.html\" title=\"trait core::iter::traits::iterator::Iterator\">Iterator</a> for <a class=\"struct\" href=\"wasmer/sys/exports/struct.ExportsIterator.html\" title=\"struct wasmer::sys::exports::ExportsIterator\">ExportsIterator</a>&lt;'a, I&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;I: <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/iter/traits/iterator/trait.Iterator.html\" title=\"trait core::iter::traits::iterator::Iterator\">Iterator</a>&lt;Item = <a class=\"primitive\" href=\"https://doc.rust-lang.org/1.59.0/std/primitive.tuple.html\">(</a>&amp;'a <a class=\"struct\" href=\"https://doc.rust-lang.org/1.59.0/alloc/string/struct.String.html\" title=\"struct alloc::string::String\">String</a>, &amp;'a <a class=\"enum\" href=\"wasmer/sys/externals/enum.Extern.html\" title=\"enum wasmer::sys::externals::Extern\">Extern</a><a class=\"primitive\" href=\"https://doc.rust-lang.org/1.59.0/std/primitive.tuple.html\">)</a>&gt; + <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/marker/trait.Sized.html\" title=\"trait core::marker::Sized\">Sized</a>,&nbsp;</span>","synthetic":false,"types":["wasmer::sys::exports::ExportsIterator"]},{"text":"impl&lt;'a, T:&nbsp;<a class=\"trait\" href=\"wasmer/trait.ValueType.html\" title=\"trait wasmer::ValueType\">ValueType</a>&gt; <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/iter/traits/iterator/trait.Iterator.html\" title=\"trait core::iter::traits::iterator::Iterator\">Iterator</a> for <a class=\"struct\" href=\"wasmer/sys/mem_access/struct.WasmSliceIter.html\" title=\"struct wasmer::sys::mem_access::WasmSliceIter\">WasmSliceIter</a>&lt;'a, T&gt;","synthetic":false,"types":["wasmer::sys::mem_access::WasmSliceIter"]}];
implementors["wasmer_vfs"] = [{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/iter/traits/iterator/trait.Iterator.html\" title=\"trait core::iter::traits::iterator::Iterator\">Iterator</a> for <a class=\"struct\" href=\"wasmer_vfs/struct.ReadDir.html\" title=\"struct wasmer_vfs::ReadDir\">ReadDir</a>","synthetic":false,"types":["wasmer_vfs::ReadDir"]}];
implementors["wasmer_wasi"] = [{"text":"impl <a class=\"trait\" href=\"https://doc.rust-lang.org/1.59.0/core/iter/traits/iterator/trait.Iterator.html\" title=\"trait core::iter::traits::iterator::Iterator\">Iterator</a> for <a class=\"struct\" href=\"wasmer_wasi/state/types/struct.PollEventIter.html\" title=\"struct wasmer_wasi::state::types::PollEventIter\">PollEventIter</a>","synthetic":false,"types":["wasmer_wasi::state::types::PollEventIter"]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()