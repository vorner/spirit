(function() {var implementors = {};
implementors["spirit"] = [];
implementors["spirit_log"] = [{"text":"impl&lt;E&gt; Extension&lt;E&gt; for FlushGuard <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;E: Extensible&lt;Ok = E&gt;,&nbsp;</span>","synthetic":false,"types":[]}];
implementors["spirit_tokio"] = [{"text":"impl&lt;E&gt; Extension&lt;E&gt; for Tokio&lt;E::Opts, E::Config&gt; <span class=\"where fmt-newline\">where<br>&nbsp;&nbsp;&nbsp;&nbsp;E: Extensible&lt;Ok = E&gt;,<br>&nbsp;&nbsp;&nbsp;&nbsp;E::Config: DeserializeOwned + Send + Sync + 'static,<br>&nbsp;&nbsp;&nbsp;&nbsp;E::Opts: StructOpt + Send + Sync + 'static,&nbsp;</span>","synthetic":false,"types":[]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()