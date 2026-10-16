param(
  [Parameter(Mandatory = $false)]
  [string]$Version = ''
)

# Do not rebuild project/dependency code on a runner carrying publication credentials.
throw 'Direct local asset publication is disabled. Use just release, or rerun the original Release workflow to retry its exact source and version.'
