(function() {var implementors = {};
implementors["spirit"] = [];
implementors["spirit_hyper"] = [{"text":"impl&lt;Tr, Inst, BS&gt; Transformation&lt;Builder&lt;Acceptor&lt;&lt;Tr as Fragment&gt;::Resource&gt;, Exec&gt;, Inst, HyperServer&lt;Tr&gt;&gt; for BuildServer&lt;BS&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;Tr: Fragment + Clone + Send + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;Tr::Resource: Send,<br>&nbsp;&nbsp;&nbsp;&nbsp;BS: ServerBuilder&lt;Tr&gt; + Clone + Send + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;BS::OutputFut: Future&lt;Output = Result&lt;(), HyperError&gt;&gt;,&nbsp;</span>","synthetic":false,"types":[]}];
implementors["spirit_log"] = [{"text":"impl&lt;I, F&gt; Transformation&lt;Dispatch, I, F&gt; for Background","synthetic":false,"types":[]}];
implementors["spirit_reqwest"] = [{"text":"impl&lt;I, F&gt; Transformation&lt;ClientBuilder, I, F&gt; for IntoClient","synthetic":false,"types":[]},{"text":"impl&lt;I, F&gt; Transformation&lt;ClientBuilder, I, F&gt; for IntoClient","synthetic":false,"types":[]}];
implementors["spirit_tokio"] = [{"text":"impl&lt;F, Fut, R, II, SF&gt; Transformation&lt;R, II, SF&gt; for ToFuture&lt;F&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;F: FnMut(R, &amp;SF) -&gt; Result&lt;Fut, AnyError&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;Fut: Future&lt;Output = ()&gt; + 'static,&nbsp;</span>","synthetic":false,"types":[]},{"text":"impl&lt;F, Fut, R, II, SF&gt; Transformation&lt;R, II, SF&gt; for ToFutureUnconfigured&lt;F&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;F: FnMut(R) -&gt; Fut,<br>&nbsp;&nbsp;&nbsp;&nbsp;Fut: Future&lt;Output = ()&gt; + 'static,&nbsp;</span>","synthetic":false,"types":[]},{"text":"impl&lt;F, Fut, A, II, SF&gt; Transformation&lt;A, II, SF&gt; for PerConnection&lt;F&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;A: Accept,<br>&nbsp;&nbsp;&nbsp;&nbsp;F: Clone + FnMut(A::Connection, &amp;SF) -&gt; Fut + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;Fut: Future&lt;Output = ()&gt; + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;SF: Clone + 'static,&nbsp;</span>","synthetic":false,"types":[]},{"text":"impl&lt;FA, FC, Fut, A, II, SF&gt; Transformation&lt;A, II, SF&gt; for PerConnectionInit&lt;FA&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;A: Accept,<br>&nbsp;&nbsp;&nbsp;&nbsp;FA: FnMut(&amp;A, &amp;SF) -&gt; FC + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;FC: FnMut(A::Connection, &amp;SF) -&gt; Fut + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;Fut: Future&lt;Output = ()&gt; + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;SF: Clone + 'static,&nbsp;</span>","synthetic":false,"types":[]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()