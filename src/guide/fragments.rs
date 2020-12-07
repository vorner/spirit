/*!
Using Fragments and Pipelines

It is possible to integrate with [`Spirit`] by registering the callbacks directly. But if one needs
to track state with that ‒ replacing an old instance of something if the configuration changed, but
not bothering if it is the same and putting it to use in the application ‒ that might be a bit of
work to do every time. This pattern, when something is created form a bit of configuration and then
put to use, is abstracted by the [`Pipeline`].

The pipeline works by going through several phases.

* First, the fragment is extracted from configuration (and/or the command line options).
* The fragment is optionally checked for equality from last time.
* If it is different (or configured to get replaced every time), a Resource is created ‒ the thing
  one is interested in.
* It might go through a series of transformations.
* Eventually, it is installed.

The idea is, this allows building a customized resources from bits of configuration. The
[`Fragment`] usually comes with enough setup, so it is enough to provide the function to extract
it, unless something unusual is needed.

Note that pipelines have names. They are used in logging.

# Composing things

Apart from tracking changes to stuff, pipelines can do one more thing. One can use an `Option` or
`Vec` of the configuration fragment (or one of few other containers). Then as many instances are
created and installed, and they are removed or added as needed.

# Customization

* The strategy of when and how to replace the instances is done through the
  [driver][crate::fragment::pipeline::Pipeline::driver].
* One can modify the instance of the resource through the
  [transform][crate::fragment::pipeline::Pipeline::transform],
  [map][crate::fragment::pipeline::Pipeline::map] or
  [and_then][crate::fragment::pipeline::Pipeline::and_then].
* How one installs is set up through [install][crate::fragment::pipeline::Pipeline::install].

# Quirks of pipelines

Pipelines use generics heavily. Furthermore, the „check“ if all the types align in them (and if
they implement the right traits) is performed at the very end. That makes it possible for the types
*not* to align on the way and enables creating of [`Fragment`]s that are not complete (for
example, they *need* some post-processing from the caller, but already want to set the installer,
which doesn't match at the beginning). The downside is, there's often too much flexibility in the
solutions for `rustc` to give reasonable hints in the error message.

The [`check`][crate::fragment::pipeline::Pipeline::check] method does nothing (it doesn't even
modify the pipeline), except that it forces the type checking at the point. Placing it at strategic
place (even at the end) might help `rustc` produce a better hint.

[`Spirit`]: crate::Spirit
[`Pipeline`]: crate::fragment::pipeline::Pipeline
[`Fragment`]: crate::fragment::Fragment
*/
